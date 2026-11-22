//! Input capture: global hotkeys (specs/02-cli-and-tui.md "Global hotkeys"), the
//! keyboard hook with its filter and secret guard, and the shell spool
//! (specs/10-capture.md).

pub mod hook;
pub mod keys;
pub mod pointer;
pub mod shell;
pub mod toast;

use std::str::FromStr;
use std::sync::mpsc;

use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyAction {
    Mark,
    Checkpoint,
    Privacy,
    Brb,
}

/// Parses bindings like `Ctrl+Alt+M`; an empty string disables that action.
pub fn parse(bindings: &[(HotkeyAction, &str)]) -> Result<Vec<(HotkeyAction, HotKey)>, String> {
    bindings
        .iter()
        .filter(|(_, s)| !s.trim().is_empty())
        .map(|&(action, s)| {
            HotKey::from_str(s)
                .map(|key| (action, key))
                .map_err(|e| format!("hotkey {s:?}: {e}"))
        })
        .collect()
}

/// Registers the hotkeys on a dedicated thread (Windows delivers them through that
/// thread's message loop) and calls `on_press` for each press. Returns once registration
/// succeeded or failed; the thread then runs for the rest of the process.
pub fn spawn(
    hotkeys: Vec<(HotkeyAction, HotKey)>,
    on_press: impl Fn(HotkeyAction) + Send + Sync + 'static,
) -> Result<(), String> {
    let (ready_tx, ready_rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("hotkeys".into())
        .spawn(move || {
            let manager = match GlobalHotKeyManager::new() {
                Ok(m) => m,
                Err(e) => return ready_tx.send(Err(e.to_string())).unwrap_or(()),
            };
            for (_, key) in &hotkeys {
                if let Err(e) = manager.register(*key) {
                    // Usually another app owns the combination.
                    return ready_tx
                        .send(Err(format!("{}: {e}", key.into_string())))
                        .unwrap_or(());
                }
            }
            GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
                if e.state == HotKeyState::Pressed
                    && let Some((action, _)) = hotkeys.iter().find(|(_, k)| k.id() == e.id)
                {
                    on_press(*action);
                }
            }));
            let _ = ready_tx.send(Ok(()));
            message_loop();
            drop(manager);
        })
        .map_err(|e| e.to_string())?;
    ready_rx
        .recv()
        .map_err(|_| "hotkey thread exited".to_owned())?
}

#[cfg(windows)]
fn message_loop() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, MSG, TranslateMessage,
    };
    // SAFETY: standard Win32 message pump on the thread that owns the hotkey window.
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

#[cfg(not(windows))]
fn message_loop() {
    // ponytail: only Windows is tested; elsewhere the manager delivers events itself
    // (X11) or needs the main thread (macOS, unsupported here).
    loop {
        std::thread::park();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default_bindings() {
        let keys = parse(&[
            (HotkeyAction::Mark, "Ctrl+Alt+M"),
            (HotkeyAction::Brb, "ctrl+alt+b"),
            (HotkeyAction::Privacy, ""),
        ])
        .unwrap();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].1, HotKey::from_str("control+alt+KeyM").unwrap());
        assert!(parse(&[(HotkeyAction::Mark, "Ctrl+Alt+Nope")]).is_err());
    }
}
