//! Secret values from the config (specs/03-config.md): `keyring:<name>` (service
//! `aftercast`), `env:<NAME>`, or a plain value. Empty means "no secret".

/// Keyring service for every aftercast secret.
pub const SERVICE: &str = "aftercast";

pub fn resolve(value: &str) -> Result<Option<String>, String> {
    if let Some(name) = value.strip_prefix("keyring:") {
        return match keyring::Entry::new(SERVICE, name).and_then(|e| e.get_password()) {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(format!("keyring {SERVICE}/{name}: {e}")),
        };
    }
    if let Some(name) = value.strip_prefix("env:") {
        return Ok(std::env::var(name).ok());
    }
    Ok(Some(value.to_owned()).filter(|v| !v.is_empty()))
}

/// Stores a secret for a `keyring:<name>` reference.
pub fn store(name: &str, secret: &str) -> Result<(), String> {
    keyring::Entry::new(SERVICE, name)
        .and_then(|e| e.set_password(secret))
        .map_err(|e| format!("keyring {SERVICE}/{name}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_env_and_empty() {
        assert_eq!(resolve("hunter2").unwrap(), Some("hunter2".into()));
        assert_eq!(resolve("").unwrap(), None);
        assert_eq!(resolve("env:AFTERCAST_TEST_SURELY_UNSET").unwrap(), None);
        // PATH is set on every platform the tests run on.
        assert!(resolve("env:PATH").unwrap().is_some());
    }
}
