//! Native desktop notifications: Windows toasts.

use std::path::PathBuf;

/// The name aftercast's notifications come from (registered below, so Windows shows it and the
/// icon instead of refusing the toast).
pub const APP_ID: &str = "aftercast";

pub struct Toast {
    /// The bold first line.
    pub title: String,
    /// The text under it.
    pub body: String,
    /// The small line at the bottom.
    pub note: String,
    /// A picture shown round on the left.
    pub image: Option<PathBuf>,
    /// Windows' notification sound; otherwise silent.
    pub sound: bool,
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// The toast's XML (what Windows draws).
pub fn xml(t: &Toast) -> String {
    let image = t.image.as_ref().map_or_else(String::new, |p| {
        let src = format!("file:///{}", p.display().to_string().replace('\\', "/"));
        format!(
            r#"<image placement="appLogoOverride" hint-crop="circle" src="{}"/>"#,
            esc(&src)
        )
    });
    let audio = if t.sound {
        r#"<audio src="ms-winsoundevent:Notification.Reminder"/>"#
    } else {
        r#"<audio silent="true"/>"#
    };
    format!(
        r#"<toast><visual><binding template="ToastGeneric">{image}<text>{}</text><text>{}</text><text placement="attribution">{}</text></binding></visual>{audio}</toast>"#,
        esc(&t.title),
        esc(&t.body),
        esc(&t.note)
    )
}

/// Shows the toast without waiting for it; `on_error` hears why it could not (once).
#[cfg(windows)]
pub fn show(t: Toast, on_error: impl FnOnce(String) + Send + 'static) {
    std::thread::spawn(move || {
        if let Err(e) = show_now(&t) {
            on_error(e);
        }
    });
}

#[cfg(not(windows))]
pub fn show(_: Toast, _: impl FnOnce(String) + Send + 'static) {}

/// Registers the app name and icon for notifications (once per run): without an entry under
/// AppUserModelId Windows drops the toast.
#[cfg(windows)]
fn register(icon: Option<&std::path::Path>) {
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let key = format!(r"HKCU\Software\Classes\AppUserModelId\{APP_ID}");
        let mut values = vec![("DisplayName", APP_ID.to_owned())];
        if let Some(icon) = icon {
            values.push(("IconUri", icon.display().to_string()));
        }
        for (name, data) in values {
            let _ = Command::new("reg")
                .args(["add", &key, "/v", name, "/t", "REG_SZ", "/d", &data, "/f"])
                .creation_flags(0x0800_0000) // no console window
                .output();
        }
    });
}

#[cfg(windows)]
fn show_now(t: &Toast) -> Result<(), String> {
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
    use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
    use windows::core::HSTRING;
    let fail = |e: windows::core::Error| e.message();
    register(t.image.as_deref());
    // SAFETY: plain COM setup for this thread; it may already be initialised, which is fine.
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
    let doc = XmlDocument::new().map_err(fail)?;
    doc.LoadXml(&HSTRING::from(xml(t))).map_err(fail)?;
    let toast = ToastNotification::CreateToastNotification(&doc).map_err(fail)?;
    ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APP_ID))
        .map_err(fail)?
        .Show(&toast)
        .map_err(fail)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs a desktop session (shows a real notification):
    /// `cargo test -p ac-capture toast -- --ignored --nocapture`.
    #[test]
    #[ignore]
    #[cfg(windows)]
    fn shows_a_notification() {
        let logo = std::env::current_dir()
            .unwrap()
            .join("../../assets/logo/logo-128.png");
        let t = Toast {
            title: "₹500.00 Super Chat".into(),
            body: "ana_codes: How do you test the tokenizer?".into(),
            note: "aftercast".into(),
            image: logo.canonicalize().ok(),
            sound: true,
        };
        show_now(&t).unwrap();
    }

    #[test]
    fn text_is_escaped_and_the_sound_chosen() {
        let t = Toast {
            title: "₹500.00 Super Chat".into(),
            body: "ana <b>&co</b>: \"hi\"".into(),
            note: "aftercast".into(),
            image: Some(PathBuf::from(r"C:\home\state\logo.png")),
            sound: false,
        };
        let x = xml(&t);
        assert!(
            x.contains("ana &lt;b&gt;&amp;co&lt;/b&gt;: &quot;hi&quot;"),
            "{x}"
        );
        assert!(x.contains(r#"src="file:///C:/home/state/logo.png""#), "{x}");
        assert!(x.contains(r#"<audio silent="true"/>"#));
        let loud = Toast { sound: true, ..t };
        assert!(xml(&loud).contains("ms-winsoundevent:Notification.Reminder"));
    }
}
