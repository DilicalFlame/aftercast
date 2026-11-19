//! OAuth for a Google "Desktop app" client: loopback redirect + PKCE, refresh token in
//! the OS keyring under `aftercast` / `youtube/<profile>`.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use reqwest::Url;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

use crate::{Error, LOGIN_HINT, Result};

const SCOPE: &str = "https://www.googleapis.com/auth/youtube.force-ssl";
const KEYRING_SERVICE: &str = "aftercast";
/// Refresh this long before the access token actually expires.
const EXPIRY_MARGIN: Duration = Duration::from_secs(60);

/// `client_secret.json` as downloaded from Google Cloud (`{"installed": {...}}`).
#[derive(Debug, Clone, Deserialize)]
pub struct ClientSecret {
    pub client_id: String,
    pub client_secret: String,
    pub auth_uri: String,
    pub token_uri: String,
}

impl ClientSecret {
    pub fn load(path: &Path) -> Result<Self> {
        #[derive(Deserialize)]
        struct File {
            installed: ClientSecret,
        }
        let text = fs::read_to_string(path).map_err(|e| {
            Error::Auth(format!(
                "{}: {e}; download the OAuth Desktop client JSON from Google Cloud",
                path.display()
            ))
        })?;
        serde_json::from_str::<File>(&text)
            .map(|f| f.installed)
            .map_err(|e| {
                Error::Auth(format!(
                    "{}: not a Desktop app client secret: {e}",
                    path.display()
                ))
            })
    }
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
    refresh_token: Option<String>,
}

fn entry(profile: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(KEYRING_SERVICE, &format!("youtube/{profile}"))
        .map_err(|e| Error::Auth(format!("keyring: {e}")))
}

/// A signed-in profile: exchanges its refresh token for access tokens as needed.
pub struct Auth {
    secret: ClientSecret,
    refresh_token: String,
    access: Mutex<Option<(String, Instant)>>,
    expires_at: std::sync::Mutex<Option<Instant>>,
}

impl Auth {
    /// Loads the client secret and the profile's refresh token from the keyring.
    pub fn load(secret_path: &Path, profile: &str) -> Result<Self> {
        let secret = ClientSecret::load(secret_path)?;
        let refresh_token = match entry(profile)?.get_password() {
            Ok(token) => token,
            Err(keyring::Error::NoEntry) => {
                return Err(Error::Auth(format!(
                    "profile {profile:?} is not signed in; {LOGIN_HINT}"
                )));
            }
            Err(e) => return Err(Error::Auth(format!("keyring: {e}"))),
        };
        Ok(Self {
            secret,
            refresh_token,
            access: Mutex::new(None),
            expires_at: std::sync::Mutex::new(None),
        })
    }

    /// A valid access token, refreshed when missing or about to expire.
    pub async fn access_token(&self, http: &reqwest::Client) -> Result<String> {
        let mut access = self.access.lock().await;
        if let Some((token, until)) = access.as_ref()
            && Instant::now() + EXPIRY_MARGIN < *until
        {
            return Ok(token.clone());
        }
        let response = token_request(
            http,
            &self.secret.token_uri,
            &[
                ("grant_type", "refresh_token"),
                ("refresh_token", &self.refresh_token),
                ("client_id", &self.secret.client_id),
                ("client_secret", &self.secret.client_secret),
            ],
        )
        .await?;
        let until = Instant::now() + Duration::from_secs(response.expires_in);
        *self.expires_at.lock().expect("expiry lock") = Some(until);
        *access = Some((response.access_token.clone(), until));
        Ok(response.access_token)
    }

    /// Time left on the current access token, if one has been fetched.
    pub fn expires_in(&self) -> Option<Duration> {
        self.expires_at
            .lock()
            .expect("expiry lock")
            .map(|until| until.saturating_duration_since(Instant::now()))
    }
}

async fn token_request(
    http: &reqwest::Client,
    uri: &str,
    form: &[(&str, &str)],
) -> Result<TokenResponse> {
    let response = http.post(uri).form(form).send().await?;
    let status = response.status();
    let body: serde_json::Value = response.json().await.unwrap_or_default();
    if !status.is_success() {
        let reason = body["error"].as_str().unwrap_or("unknown");
        let hint = if reason == "invalid_grant" {
            format!(
                "; the refresh token expired or was revoked ({LOGIN_HINT}). If the OAuth consent                  screen is in Testing, tokens expire after ~7 days: set it to In production"
            )
        } else {
            String::new()
        };
        return Err(Error::Auth(format!(
            "token request failed ({status}): {reason}{hint}"
        )));
    }
    serde_json::from_value(body).map_err(|e| Error::Auth(format!("unexpected token response: {e}")))
}

fn random_b64(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("OS randomness");
    URL_SAFE_NO_PAD.encode(buf)
}

/// Interactive login: opens the consent page, receives the code on a loopback port,
/// exchanges it and stores the refresh token in the keyring.
pub async fn login(secret_path: &Path, profile: &str) -> Result<()> {
    let secret = ClientSecret::load(secret_path)?;
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| Error::Auth(format!("loopback listener: {e}")))?;
    let port = listener
        .local_addr()
        .map_err(|e| Error::Auth(e.to_string()))?
        .port();
    let redirect = format!("http://127.0.0.1:{port}");
    let verifier = random_b64(32);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = random_b64(16);
    let url = Url::parse_with_params(
        &secret.auth_uri,
        [
            ("client_id", secret.client_id.as_str()),
            ("redirect_uri", &redirect),
            ("response_type", "code"),
            ("scope", SCOPE),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
            ("state", &state),
            ("access_type", "offline"),
            ("prompt", "consent"),
        ],
    )
    .map_err(|e| Error::Auth(format!("auth_uri: {e}")))?;

    println!("Opening the Google consent page. If no browser opens, visit:\n{url}\n");
    if let Err(e) = open::that(url.as_str()) {
        tracing::warn!("could not open a browser: {e}");
    }

    let code = receive_code(&listener, &state).await?;
    let http = reqwest::Client::new();
    let response = token_request(
        &http,
        &secret.token_uri,
        &[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", &redirect),
            ("client_id", &secret.client_id),
            ("client_secret", &secret.client_secret),
            ("code_verifier", &verifier),
        ],
    )
    .await?;
    let refresh = response.refresh_token.ok_or_else(|| {
        Error::Auth("Google returned no refresh token; revoke the app's access and retry".into())
    })?;
    entry(profile)?
        .set_password(&refresh)
        .map_err(|e| Error::Auth(format!("keyring: {e}")))
}

/// Accepts the browser redirect and returns the authorization code.
async fn receive_code(listener: &TcpListener, expected_state: &str) -> Result<String> {
    loop {
        let (mut stream, _) = listener
            .accept()
            .await
            .map_err(|e| Error::Auth(e.to_string()))?;
        let mut buf = vec![0u8; 8192];
        let n = stream.read(&mut buf).await.unwrap_or(0);
        let request = String::from_utf8_lossy(&buf[..n]);
        let Some(target) = request
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
        else {
            continue;
        };
        let Ok(url) = Url::parse(&format!("http://127.0.0.1{target}")) else {
            continue;
        };
        let param = |name: &str| {
            url.query_pairs()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.into_owned())
        };
        let (message, result) = match (param("code"), param("state"), param("error")) {
            (Some(code), Some(state), _) if state == expected_state => {
                ("Signed in. You can close this tab.", Ok(code))
            }
            (_, _, Some(error)) => (
                "Sign-in failed. See the terminal.",
                Err(Error::Auth(format!("consent denied: {error}"))),
            ),
            // Favicon requests and the like: keep waiting for the redirect.
            _ => continue,
        };
        let body = format!("<!doctype html><title>aftercast</title><p>{message}</p>");
        let reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(reply.as_bytes()).await;
        return result;
    }
}

/// Removes the profile's refresh token (revoking it at Google, best effort).
/// Returns false when the profile was not signed in.
pub async fn logout(profile: &str) -> Result<bool> {
    let entry = entry(profile)?;
    let token = match entry.get_password() {
        Ok(token) => token,
        Err(keyring::Error::NoEntry) => return Ok(false),
        Err(e) => return Err(Error::Auth(format!("keyring: {e}"))),
    };
    let revoke = reqwest::Client::new()
        .post("https://oauth2.googleapis.com/revoke")
        .form(&[("token", token.as_str())])
        .send()
        .await;
    if let Err(e) = revoke {
        tracing::warn!("token revoke failed (removed locally anyway): {e}");
    }
    entry
        .delete_credential()
        .map_err(|e| Error::Auth(format!("keyring: {e}")))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_matches_rfc7636_example() {
        // RFC 7636 appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_eq!(random_b64(32).len(), 43);
    }

    #[test]
    fn client_secret_requires_installed_client() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("client_secret.json");
        fs::write(
            &path,
            r#"{"installed":{"client_id":"id","client_secret":"s","auth_uri":"https://a","token_uri":"https://t","redirect_uris":["http://localhost"]}}"#,
        )
        .unwrap();
        assert_eq!(ClientSecret::load(&path).unwrap().client_id, "id");
        fs::write(&path, r#"{"web":{}}"#).unwrap();
        assert!(matches!(ClientSecret::load(&path), Err(Error::Auth(_))));
    }

    #[tokio::test]
    async fn loopback_receives_code_and_checks_state() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let browser = tokio::spawn(async move {
            for path in [
                "/favicon.ico",
                "/?state=wrong&code=x",
                "/?state=s1&code=c0de",
            ] {
                let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
                    .await
                    .unwrap();
                s.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
                    .await
                    .unwrap();
                let mut reply = String::new();
                let _ = s.read_to_string(&mut reply).await;
            }
        });
        assert_eq!(receive_code(&listener, "s1").await.unwrap(), "c0de");
        browser.await.unwrap();
    }
}
