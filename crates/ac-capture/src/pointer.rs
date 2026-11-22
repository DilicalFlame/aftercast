//! The mouse pointer and the screens it moves over.

/// One screen, as Windows lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    /// `\.\DISPLAY2`: the number Windows Settings shows is the one at the end.
    pub name: String,
    /// The monitor's interface path, which OBS stores as `monitor_id` in a display capture.
    pub id: String,
    /// Left, top, right, bottom in desktop pixels.
    pub rect: [i32; 4],
}

impl Screen {
    /// The number in `\.\DISPLAY2`.
    pub fn number(&self) -> Option<u8> {
        self.name
            .to_ascii_uppercase()
            .strip_prefix(r"\.\DISPLAY")
            .and_then(|n| n.parse().ok())
    }

    /// The pointer as fractions (0 to 1) of this screen's width and height; outside 0 to 1
    /// when the pointer is elsewhere.
    pub fn fraction(&self, x: i32, y: i32) -> (f32, f32) {
        let [l, t, r, b] = self.rect;
        (
            (x - l) as f32 / (r - l).max(1) as f32,
            (y - t) as f32 / (b - t).max(1) as f32,
        )
    }
}

/// Which screen holds the camera's scene: `number` (from `[obs.facecam] monitor`, 0 for
/// the one OBS captures) names a `\.\DISPLAYn`; else the display capture's `monitor_id`,
/// else its `monitor` index in Windows's listing; else the primary screen (at 0, 0).
pub fn choose<'a>(
    screens: &'a [Screen],
    number: u8,
    obs_id: Option<&str>,
    obs_index: Option<usize>,
) -> Option<&'a Screen> {
    let by_number = || screens.iter().find(|s| s.number() == Some(number));
    let by_id = || {
        let id = obs_id.filter(|i| !i.is_empty())?;
        screens.iter().find(|s| s.id.eq_ignore_ascii_case(id))
    };
    let by_index = || obs_index.and_then(|i| screens.get(i));
    let primary = || {
        screens
            .iter()
            .find(|s| s.rect[0] == 0 && s.rect[1] == 0)
            .or(screens.first())
    };
    if number > 0 {
        by_number().or_else(primary)
    } else {
        by_id().or_else(by_index).or_else(primary)
    }
}

/// The pointer in desktop pixels.
#[cfg(windows)]
pub fn pointer() -> Option<(i32, i32)> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut p = POINT { x: 0, y: 0 };
    // SAFETY: `p` is a valid POINT for GetCursorPos to fill.
    (unsafe { GetCursorPos(&mut p) } != 0).then_some((p.x, p.y))
}

#[cfg(not(windows))]
pub fn pointer() -> Option<(i32, i32)> {
    None
}

/// Every screen, in the order Windows lists them.
#[cfg(windows)]
pub fn screens() -> Vec<Screen> {
    use std::mem::{size_of, zeroed};
    use windows_sys::Win32::Foundation::{LPARAM, RECT};
    use windows_sys::Win32::Graphics::Gdi::{
        DISPLAY_DEVICEW, EnumDisplayDevicesW, EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR,
        MONITORINFO, MONITORINFOEXW,
    };

    /// EDD_GET_DEVICE_INTERFACE_NAME (not in windows-sys): the monitor's interface path.
    const EDD_GET_DEVICE_INTERFACE_NAME: u32 = 1;

    fn text(wide: &[u16]) -> String {
        let end = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
        String::from_utf16_lossy(&wide[..end])
    }

    unsafe extern "system" fn each(m: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> i32 {
        // SAFETY: `data` is the Vec passed to EnumDisplayMonitors below, alive for the call;
        // the structs are plain data that Windows fills after their size is set.
        unsafe {
            let out = &mut *(data as *mut Vec<Screen>);
            let mut info: MONITORINFOEXW = zeroed();
            info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
            if GetMonitorInfoW(m, (&mut info as *mut MONITORINFOEXW).cast::<MONITORINFO>()) == 0 {
                return 1;
            }
            let mut dd: DISPLAY_DEVICEW = zeroed();
            dd.cb = size_of::<DISPLAY_DEVICEW>() as u32;
            let id = if EnumDisplayDevicesW(
                info.szDevice.as_ptr(),
                0,
                &mut dd,
                EDD_GET_DEVICE_INTERFACE_NAME,
            ) != 0
            {
                text(&dd.DeviceID)
            } else {
                String::new()
            };
            let r = info.monitorInfo.rcMonitor;
            out.push(Screen {
                name: text(&info.szDevice),
                id,
                rect: [r.left, r.top, r.right, r.bottom],
            });
        }
        1
    }

    let mut screens: Vec<Screen> = Vec::new();
    // SAFETY: the callback only runs during this call and `screens` outlives it.
    unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(each),
            (&mut screens as *mut Vec<Screen>) as LPARAM,
        );
    }
    screens
}

#[cfg(not(windows))]
pub fn screens() -> Vec<Screen> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(name: &str, id: &str, rect: [i32; 4]) -> Screen {
        Screen {
            name: name.into(),
            id: id.into(),
            rect,
        }
    }

    #[test]
    fn picks_the_screen_obs_captures() {
        let screens = [
            screen(r"\.\DISPLAY1", "A", [0, 0, 1920, 1080]),
            screen(r"\.\DISPLAY2", "B", [1920, 0, 3840, 1080]),
        ];
        let id = |s: &Screen| s.id.clone();
        // OBS's monitor_id wins, then its index, then the primary screen.
        assert_eq!(
            choose(&screens, 0, Some("b"), Some(0)).map(id),
            Some("B".into())
        );
        assert_eq!(choose(&screens, 0, None, Some(1)).map(id), Some("B".into()));
        assert_eq!(choose(&screens, 0, None, None).map(id), Some("A".into()));
        // A number from the settings names \.\DISPLAYn and overrides OBS.
        assert_eq!(
            choose(&screens, 2, Some("A"), None).map(id),
            Some("B".into())
        );
        assert_eq!(choose(&screens, 9, None, None).map(id), Some("A".into()));
        assert_eq!(screens[1].fraction(2880, 540), (0.5, 0.5));
        assert!(screens[0].fraction(2880, 540).0 > 1.0);
    }

    /// Needs a desktop session: `cargo test -p ac-capture pointer -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn reads_the_pointer_and_screens() {
        println!("{:?} {:?}", pointer(), screens());
        assert_eq!(pointer().is_some(), cfg!(windows));
    }
}
