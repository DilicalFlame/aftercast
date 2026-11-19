//! The global keyboard hook (`rdev`, a `WH_KEYBOARD_LL` hook on Windows) and the
//! foreground app lookup. Raw presses leave here unfiltered; [`crate::keys::Filter`]
//! decides what is kept.

use std::sync::Arc;
use std::sync::mpsc;

use ac_core::time::{Clock, Timestamp};
use rdev::{EventType, Key};

use crate::keys::{MODS, RawKey};

/// Starts the hook thread and a worker that adds the foreground app and, for apps not in
/// `trusted`, whether a password box has focus. `on_key` runs on the worker for every key
/// press. Returns an error only if the threads could not start; a hook that fails later is
/// reported through `on_error`.
pub fn spawn(
    clock: Arc<Clock>,
    trusted: Vec<String>,
    on_key: impl Fn(RawKey) + Send + 'static,
    on_error: impl Fn(String) + Send + 'static,
) -> Result<(), String> {
    let (tx, rx) = mpsc::channel::<(Timestamp, Key, Option<String>, Vec<&'static str>)>();
    std::thread::Builder::new()
        .name("key-hook".into())
        .spawn(move || {
            let mut held: Vec<Key> = Vec::new();
            // The callback runs inside the OS hook: only bookkeeping and a channel send.
            let result = rdev::listen(move |event| match event.event_type {
                EventType::KeyPress(key) => {
                    if !held.contains(&key) {
                        held.push(key);
                    }
                    let _ = tx.send((clock.now_ts(), key, event.name, mods(&held)));
                }
                EventType::KeyRelease(key) => held.retain(|k| *k != key),
                _ => {}
            });
            if let Err(e) = result {
                on_error(format!("keyboard hook: {e:?}"));
            }
        })
        .map_err(|e| e.to_string())?;
    std::thread::Builder::new()
        .name("key-worker".into())
        .spawn(move || {
            let trusted: Vec<String> = trusted.iter().map(|a| a.to_lowercase()).collect();
            let probe = PasswordProbe::new();
            let mut was_password = false;
            for (t, key, name, mods) in rx {
                // Looked up per key (well under a millisecond), so the first keys after
                // Alt+Tab belong to the right app.
                let (app, title) = foreground();
                // Unknown counts as a password box. The previous key's answer counts too: the
                // worker may run after focus already moved on (Tab out of a password box).
                let now_password = !trusted.contains(&app.to_lowercase())
                    && probe.focused_is_password().unwrap_or(true);
                let password = now_password || was_password;
                was_password = now_password;
                let ch = match key {
                    Key::Return | Key::KpReturn => Some('\n'),
                    Key::Tab => Some('\t'),
                    _ => name.and_then(|n| {
                        let mut chars = n.chars();
                        chars.next().filter(|_| chars.next().is_none())
                    }),
                };
                on_key(RawKey {
                    t,
                    key: format!("{key:?}"),
                    ch,
                    mods,
                    app,
                    title,
                    password,
                });
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn mods(held: &[Key]) -> Vec<&'static str> {
    let on = |keys: &[Key]| keys.iter().any(|k| held.contains(k));
    let flags = [
        on(&[Key::ControlLeft, Key::ControlRight]),
        on(&[Key::Alt, Key::AltGr]),
        on(&[Key::ShiftLeft, Key::ShiftRight]),
        on(&[Key::MetaLeft, Key::MetaRight]),
    ];
    MODS.iter()
        .zip(flags)
        .filter(|(_, on)| *on)
        .map(|(m, _)| *m)
        .collect()
}

/// Asks UI Automation whether the focused element is a password box.
struct PasswordProbe {
    #[cfg(windows)]
    uia: Option<windows::Win32::UI::Accessibility::IUIAutomation>,
}

#[cfg(windows)]
impl PasswordProbe {
    fn new() -> Self {
        use windows::Win32::System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
        };
        use windows::Win32::UI::Accessibility::{CUIAutomation, IUIAutomation, IUIAutomation2};
        use windows::core::Interface;
        // SAFETY: COM is initialised once on this worker thread before any COM call.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let uia: Option<IUIAutomation> =
                CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok();
            // A hung app must not stall key delivery for long.
            if let Some(uia2) = uia.as_ref().and_then(|u| u.cast::<IUIAutomation2>().ok()) {
                let _ = uia2.SetConnectionTimeout(500);
                let _ = uia2.SetTransactionTimeout(500);
            }
            Self { uia }
        }
    }

    /// `None` when UI Automation cannot tell.
    fn focused_is_password(&self) -> Option<bool> {
        let uia = self.uia.as_ref()?;
        // SAFETY: plain COM calls on interfaces owned by this thread.
        unsafe {
            let element = uia.GetFocusedElement().ok()?;
            element.CurrentIsPassword().ok().map(|b| b.as_bool())
        }
    }
}

#[cfg(not(windows))]
impl PasswordProbe {
    fn new() -> Self {
        Self {}
    }

    fn focused_is_password(&self) -> Option<bool> {
        // ponytail: no password-box detection off Windows; deny lists still apply.
        Some(false)
    }
}

/// Process name and window title of the foreground window; empty when unknown.
#[cfg(windows)]
pub fn foreground() -> (String, String) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId,
    };
    // SAFETY: plain Win32 queries with buffers sized as passed; the process handle is
    // closed before returning.
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return (String::new(), String::new());
        }
        let mut buf = [0u16; 512];
        let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        let title = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return (String::new(), title);
        }
        let mut len = buf.len() as u32;
        let ok =
            QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len);
        CloseHandle(process);
        let path = if ok != 0 {
            String::from_utf16_lossy(&buf[..len as usize])
        } else {
            String::new()
        };
        let app = path
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or_default()
            .to_owned();
        (app, title)
    }
}

#[cfg(not(windows))]
pub fn foreground() -> (String, String) {
    // ponytail: only Windows is supported; elsewhere no app is known, so only shortcuts
    // are kept (rule 4).
    (String::new(), String::new())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// Needs a desktop session: `cargo test -p ac-capture -- --ignored`.
    #[test]
    #[ignore]
    fn ui_automation_answers_for_the_focused_window() {
        let probe = PasswordProbe::new();
        assert!(probe.uia.is_some(), "UI Automation unavailable");
        assert_eq!(probe.focused_is_password(), Some(false));
        let (app, _) = foreground();
        assert!(!app.is_empty());
        println!("foreground app: {app}");
    }
}
