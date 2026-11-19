//! The global keyboard hook (`rdev`, a `WH_KEYBOARD_LL` hook on Windows) and the
//! foreground app lookup. Raw presses leave here unfiltered; [`crate::keys::Filter`]
//! decides what is kept.

use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use ac_core::time::{Clock, Timestamp};
use rdev::{EventType, Key};

use crate::keys::{MODS, RawKey};

/// The foreground app is looked up at most this often.
const FOREGROUND_CACHE: Duration = Duration::from_millis(200);

/// Starts the hook thread and a worker that adds the foreground app. `on_key` runs on the
/// worker for every key press. Returns an error only if the threads could not start; a
/// hook that fails later is reported through `on_error`.
pub fn spawn(
    clock: Arc<Clock>,
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
            let mut cached: Option<(Instant, String, String)> = None;
            for (t, key, name, mods) in rx {
                let (app, title) = match &cached {
                    Some((at, app, title)) if at.elapsed() < FOREGROUND_CACHE => {
                        (app.clone(), title.clone())
                    }
                    _ => {
                        let (app, title) = foreground();
                        cached = Some((Instant::now(), app.clone(), title.clone()));
                        (app, title)
                    }
                };
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
