//! The Win32 boundary: sample once, establish the HWND's target DPI while
//! hidden, then place and reveal the ring without activating it.

use anyhow::{Context as _, Result, ensure};
use gpui::{App, Bounds, DevicePixels, DisplayId, point, size};
use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
use windows_sys::Win32::Foundation::{GetLastError, HWND, POINT, SetLastError};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, MONITORINFO,
    MonitorFromPoint, MonitorFromWindow,
};
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GWL_STYLE, GetCursorPos, GetWindowLongW, GetWindowRect, SWP_FRAMECHANGED, SWP_NOACTIVATE,
    SWP_NOZORDER, SWP_SHOWWINDOW, SetWindowLongW, SetWindowPos, WS_CAPTION, WS_POPUP,
    WS_THICKFRAME,
};

use super::RingPlacement;

impl RingPlacement {
    #[expect(
        unsafe_code,
        reason = "read-only Win32 cursor and monitor queries with checked outputs"
    )]
    pub(crate) fn capture(_cx: &mut App) -> Result<Self> {
        let mut cursor = POINT::default();
        // SAFETY: cursor is writable for the duration of the call. This GPUI
        // process is PerMonitorV2-aware, so GetCursorPos returns physical pixels.
        let has_cursor = unsafe { GetCursorPos(&raw mut cursor) } != 0;
        let monitor = if has_cursor {
            // SAFETY: POINT is passed by value. Use this same sample for both
            // selection and positioning, never a second cursor read.
            unsafe { MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST) }
        } else {
            // SAFETY: a null HWND plus DEFAULTTOPRIMARY selects the primary.
            unsafe { MonitorFromWindow(std::ptr::null_mut(), MONITOR_DEFAULTTOPRIMARY) }
        };
        let mut info = MONITORINFO {
            cbSize: u32::try_from(std::mem::size_of::<MONITORINFO>())?,
            ..MONITORINFO::default()
        };
        // SAFETY: monitor is from a native query and info has the required size;
        // a removed monitor is reported as failure, not used as geometry.
        if unsafe { GetMonitorInfoW(monitor, &raw mut info) } == 0 {
            return Err(std::io::Error::last_os_error()).context("query ring monitor bounds");
        }
        let rect = info.rcMonitor;
        let display = Bounds::new(
            point(DevicePixels(rect.left), DevicePixels(rect.top)),
            size(
                DevicePixels(rect.right - rect.left),
                DevicePixels(rect.bottom - rect.top),
            ),
        );
        Ok(Self {
            // The pinned GPUI Windows backend uses HMONITOR as DisplayId.
            display_id: DisplayId::from(monitor as usize as u64),
            cursor: if has_cursor {
                point(DevicePixels(cursor.x), DevicePixels(cursor.y))
            } else {
                display.center()
            },
            display,
        })
    }

    #[expect(
        unsafe_code,
        reason = "borrow the live GPUI HWND on its owning UI thread"
    )]
    pub(crate) fn show(
        self,
        handle: gpui::WindowHandle<crate::ring::RingView>,
        cx: &mut gpui::AsyncApp,
    ) -> Result<()> {
        // Return the native handle before invoking Win32: SetWindowPos sends
        // synchronous resize/move callbacks that need to borrow App themselves.
        let hwnd = handle.update(cx, |_, window, _| -> Result<HWND> {
            let RawWindowHandle::Win32(handle) = window
                .window_handle()
                .map_err(|error| anyhow::anyhow!("get ring HWND: {error}"))?
                .as_raw()
            else {
                anyhow::bail!("ring window has no Win32 handle");
            };
            Ok(handle.hwnd.get() as HWND)
        })??;
        // No await or message-pump gap: the window remains owned on this thread.
        // GPUI creates PopUp with a zero style, which Windows promotes to an
        // overlapped window with a caption/non-client frame. Establish a real
        // popup before DPI negotiation so the requested bounds are all client area.
        set_borderless_popup(hwnd)?;
        // Bootstrap a tiny hidden window well inside the selected monitor.
        // SetWindowPos synchronously delivers WM_DPICHANGED; GPUI applies its
        // suggested rectangle and updates rendering scale before this returns.
        // Final geometry is applied only AFTER that transition, so the suggested
        // rectangle cannot move a cursor-centred ring away from its anchor.
        let anchor = Bounds::new(
            self.display.center(),
            size(DevicePixels(1), DevicePixels(1)),
        );
        set_bounds(
            hwnd,
            anchor,
            SWP_NOACTIVATE | SWP_NOZORDER | SWP_FRAMECHANGED,
        )?;
        // SAFETY: hwnd belongs to the still-borrowed window.
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        ensure!(dpi != 0, "could not read ring window DPI");
        // SAFETY: read-only query on the still-borrowed window.
        let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
        ensure!(
            monitor as usize as u64 == u64::from(self.display_id),
            "ring monitor changed during window creation"
        );
        let picker = handle.update(cx, |view, _, _| view.is_window_picker())?;
        let show_flags = if picker {
            SWP_NOZORDER | SWP_SHOWWINDOW
        } else {
            SWP_NOACTIVATE | SWP_NOZORDER | SWP_SHOWWINDOW
        };
        set_bounds(hwnd, self.bounds(dpi), show_flags)?;
        let mut rect = windows_sys::Win32::Foundation::RECT::default();
        ensure!(
            // SAFETY: the window is still owned on this thread and rect is writable.
            unsafe { GetWindowRect(hwnd, &raw mut rect) } != 0,
            "could not read ring input region"
        );
        handle.update(cx, |view, window, cx| {
            view.publish_input_region(openlogi_core::action_ring::InputRegion {
                x: f64::from(rect.left),
                y: f64::from(rect.top),
                width: f64::from(rect.right - rect.left),
                height: f64::from(rect.bottom - rect.top),
            });
            view.present(window, cx);
        })
    }
}

#[expect(
    unsafe_code,
    reason = "SetWindowPos on a borrowed HWND, with physical screen bounds"
)]
fn set_bounds(hwnd: HWND, bounds: Bounds<DevicePixels>, flags: u32) -> Result<()> {
    // SAFETY: the caller holds the live GPUI window on its owning thread;
    // NOZORDER makes the null insert-after handle irrelevant.
    if unsafe {
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            bounds.origin.x.0,
            bounds.origin.y.0,
            bounds.size.width.0,
            bounds.size.height.0,
            flags,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error()).context("position Actions Ring window");
    }
    Ok(())
}

const fn popup_style(style: u32) -> u32 {
    (style & !(WS_CAPTION | WS_THICKFRAME)) | WS_POPUP
}

#[expect(
    unsafe_code,
    reason = "read/write the style of a live GPUI HWND borrowed on its UI thread"
)]
fn set_borderless_popup(hwnd: HWND) -> Result<()> {
    // SAFETY: the caller borrows a live top-level HWND. GWL_STYLE is a 32-bit
    // value on both Windows architectures, not a pointer or window procedure.
    let (style, error) = unsafe {
        SetLastError(0);
        let style = GetWindowLongW(hwnd, GWL_STYLE);
        (style, GetLastError())
    };
    if style == 0 && error != 0 {
        return Err(std::io::Error::from_raw_os_error(error.cast_signed()))
            .context("read ring window style");
    }
    let style = popup_style(style.cast_unsigned()).cast_signed();
    // SAFETY: same borrowed HWND and 32-bit style slot. A zero previous style
    // is valid, so clear/read last-error rather than treating zero as failure.
    let (previous, error) = unsafe {
        SetLastError(0);
        let previous = SetWindowLongW(hwnd, GWL_STYLE, style);
        (previous, GetLastError())
    };
    if previous == 0 && error != 0 {
        return Err(std::io::Error::from_raw_os_error(error.cast_signed()))
            .context("remove ring window frame");
    }
    Ok(())
}

#[expect(
    unsafe_code,
    reason = "set click-through on the borrowed live overlay HWND"
)]
pub(crate) fn release_pointer(window: &gpui::Window) -> Result<()> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GWL_EXSTYLE, WS_EX_TRANSPARENT};
    let RawWindowHandle::Win32(handle) = raw_window_handle::HasWindowHandle::window_handle(window)
        .map_err(|error| anyhow::anyhow!("get ring HWND: {error}"))?
        .as_raw()
    else {
        anyhow::bail!("ring window has no Win32 handle");
    };
    let hwnd = handle.hwnd.get() as HWND;
    // SAFETY: the live window is borrowed on its owning thread; only the input
    // transparency bit changes. No geometry API or synchronous resize is invoked.
    let error = unsafe {
        SetLastError(0);
        let style = GetWindowLongW(hwnd, GWL_EXSTYLE);
        let error = GetLastError();
        if style == 0 && error != 0 {
            return Err(std::io::Error::from_raw_os_error(error.cast_signed()))
                .context("read ring extended style");
        }
        SetLastError(0);
        SetWindowLongW(hwnd, GWL_EXSTYLE, style | WS_EX_TRANSPARENT.cast_signed());
        GetLastError()
    };
    if error != 0 {
        return Err(std::io::Error::from_raw_os_error(error.cast_signed()))
            .context("release ring pointer input");
    }
    Ok(())
}

#[expect(
    unsafe_code,
    reason = "hide the owned HWND after ending the GPUI borrow"
)]
pub(crate) fn hide(handle: gpui::AnyWindowHandle, cx: &mut gpui::AsyncApp) -> Result<()> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, ShowWindow};
    let hwnd = handle.update(cx, |_, window, _| -> Result<HWND> {
        let RawWindowHandle::Win32(handle) = window
            .window_handle()
            .map_err(|error| anyhow::anyhow!("get ring HWND: {error}"))?
            .as_raw()
        else {
            anyhow::bail!("ring window has no Win32 handle");
        };
        Ok(handle.hwnd.get() as HWND)
    })??;
    // SAFETY: no yield since borrowing the handle; callbacks may now borrow App.
    unsafe {
        ShowWindow(hwnd, SW_HIDE);
    }
    Ok(())
}

/// Move the cursor in physical screen pixels, the space the ring's input
/// region is published in. This process is PerMonitorV2-aware, so no further
/// DPI translation applies.
#[expect(
    unsafe_code,
    reason = "SetCursorPos takes screen coordinates by value and no buffer"
)]
#[expect(
    clippy::cast_possible_truncation,
    reason = "screen coordinates are bounded by the virtual desktop"
)]
pub(crate) fn warp_cursor(x: f64, y: f64) -> Result<()> {
    use windows_sys::Win32::UI::WindowsAndMessaging::SetCursorPos;
    ensure!(
        x.is_finite() && y.is_finite(),
        "cursor target is not finite"
    );
    // SAFETY: no pointer arguments; the call moves only the cursor position.
    if unsafe { SetCursorPos(x.round() as i32, y.round() as i32) } == 0 {
        return Err(std::io::Error::last_os_error()).context("move cursor to ring slot");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::{WS_CLIPSIBLINGS, WS_VISIBLE};

    #[test]
    fn popup_removes_non_client_frame_and_preserves_unrelated_styles() {
        let style = popup_style(WS_CAPTION | WS_THICKFRAME | WS_VISIBLE | WS_CLIPSIBLINGS);
        assert_eq!(style, WS_POPUP | WS_VISIBLE | WS_CLIPSIBLINGS);
        assert_eq!(popup_style(style), style);
    }

    #[test]
    fn zero_style_becomes_an_explicit_popup_not_an_overlapped_window() {
        assert_eq!(popup_style(0), WS_POPUP);
    }
}
