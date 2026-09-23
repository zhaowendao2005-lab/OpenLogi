//! Win32 top-level window enumeration, grouped by executable rather than PID.
#![expect(
    unsafe_code,
    reason = "Win32 enumeration callback and process/window queries"
)]

use std::collections::HashMap;

use windows_sys::Win32::Foundation::{HWND, LPARAM};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GWL_EXSTYLE, GetWindow, GetWindowLongW, GetWindowTextW, IsIconic,
    IsWindowVisible, WS_EX_TOOLWINDOW,
};

use super::{AppWindow, WindowTarget};

struct Enumeration {
    executable: String,
    paths: HashMap<u32, Option<String>>,
    windows: Vec<AppWindow>,
}

pub(super) fn list(target: WindowTarget) -> std::io::Result<Vec<AppWindow>> {
    let app = target
        .window
        .executable_identity()
        .ok_or_else(std::io::Error::last_os_error)?;
    let mut state = Enumeration {
        executable: app,
        paths: HashMap::new(),
        windows: Vec::new(),
    };

    // SAFETY: EnumWindows invokes the callback synchronously while `state` is
    // borrowed exclusively; no pointer escapes the call.
    if unsafe { EnumWindows(Some(collect), (&raw mut state) as LPARAM) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(state.windows)
}

unsafe extern "system" fn collect(hwnd: HWND, data: LPARAM) -> i32 {
    // SAFETY: `data` is the address of the live `Enumeration` passed to the
    // synchronous EnumWindows call above.
    let state = unsafe { &mut *(data as *mut Enumeration) };

    // SAFETY: this query accepts a possibly expired handle and fails closed.
    let visible = unsafe { IsWindowVisible(hwnd) } != 0;
    // SAFETY: this query accepts a possibly expired handle and fails closed.
    let style = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) }.cast_unsigned();
    // SAFETY: this query accepts a possibly expired handle and fails closed.
    let owner = unsafe { GetWindow(hwnd, GW_OWNER) };
    if !visible || style & WS_EX_TOOLWINDOW != 0 || !owner.is_null() {
        return 1;
    }

    let Some(window) = crate::ForegroundWindow::from_handle(hwnd) else {
        return 1;
    };
    let (_, process) = window.identity();
    if state
        .paths
        .entry(process)
        .or_insert_with(|| window.executable_identity())
        .as_ref()
        != Some(&state.executable)
    {
        return 1;
    }

    let mut title = [0_u16; 4096];
    // SAFETY: GetWindowTextW reads a foreign top-level caption without sending
    // messages to that application; the buffer has exactly 4096 elements.
    let length = unsafe { GetWindowTextW(hwnd, title.as_mut_ptr(), 4096) };
    let title = if length > 0 {
        let length = usize::try_from(length).unwrap_or(0);
        String::from_utf16_lossy(&title[..length])
    } else {
        // Untitled top-level windows are still real application instances and
        // must remain selectable. The executable is a stable useful fallback.
        state.executable.clone()
    };
    state.windows.push(AppWindow {
        target: WindowTarget { window },
        executable: state.executable.clone(),
        title,
        // SAFETY: IsIconic accepts a possibly expired handle and fails closed.
        minimized: unsafe { IsIconic(hwnd) } != 0,
    });
    1
}
