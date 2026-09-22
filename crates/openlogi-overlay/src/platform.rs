//! Native window policy for the standalone Actions Ring overlay.

#[cfg(not(target_os = "windows"))]
mod placement;
// Keep Windows geometry tests runnable on the host; only native.rs needs Win32.
#[cfg(any(target_os = "windows", test))]
mod windows;

#[cfg(not(target_os = "windows"))]
pub(crate) use placement::RingPlacement;
#[cfg(target_os = "windows")]
pub(crate) use windows::RingPlacement;

/// Keep the overlay out of the Dock and app switcher.
#[cfg(target_os = "macos")]
pub fn configure_application() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

    if let Some(marker) = MainThreadMarker::new() {
        NSApplication::sharedApplication(marker)
            .setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    }
}

/// Make the transparent ring panel borderless and remove its native shadow.
#[cfg(target_os = "macos")]
pub fn configure_windows() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSWindowStyleMask};

    if let Some(marker) = MainThreadMarker::new() {
        for window in NSApplication::sharedApplication(marker).windows() {
            window.setStyleMask(NSWindowStyleMask::NonactivatingPanel);
            window.setHasShadow(false);
        }
    }
}

/// No native application policy is required away from macOS.
#[cfg(not(target_os = "macos"))]
pub fn configure_application() {}

/// Linux needs no additional native window configuration here.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn configure_windows() {}

/// Owner of the native click-away event monitor; dropping it removes the
/// monitor. Create and drop on the main thread.
#[cfg(target_os = "macos")]
pub struct ClickAwayMonitor(objc2::rc::Retained<objc2::runtime::AnyObject>);

#[cfg(target_os = "macos")]
impl Drop for ClickAwayMonitor {
    #[expect(
        unsafe_code,
        reason = "NSEvent::removeMonitor is plain AppKit FFI; the token is exactly what addGlobalMonitor returned"
    )]
    fn drop(&mut self) {
        // SAFETY: `self.0` is the monitor token returned by
        // `addGlobalMonitorForEventsMatchingMask_handler`, removed only once.
        unsafe { objc2_app_kit::NSEvent::removeMonitor(&self.0) };
    }
}

/// Invoke `on_mouse_down` for every mouse-down that macOS delivers to *other*
/// applications, for as long as the returned monitor is held.
///
/// Global `NSEvent` monitors never see events routed to this process's own
/// windows and cannot consume the events they observe — together exactly the
/// ring's click-away contract: clicks on the ring keep hitting the GPUI
/// handlers they always did, while a click anywhere else can dismiss the ring
/// without being swallowed. Must be called on the main thread (returns `None`
/// off it); the handler runs on the main run loop.
#[cfg(target_os = "macos")]
pub fn watch_clicks_outside(on_mouse_down: impl Fn() + 'static) -> Option<ClickAwayMonitor> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSEvent, NSEventMask};

    MainThreadMarker::new()?;
    let handler: block2::RcBlock<dyn Fn(std::ptr::NonNull<NSEvent>)> =
        block2::RcBlock::new(move |_event| on_mouse_down());
    NSEvent::addGlobalMonitorForEventsMatchingMask_handler(
        NSEventMask::LeftMouseDown | NSEventMask::RightMouseDown | NSEventMask::OtherMouseDown,
        &handler,
    )
    .map(ClickAwayMonitor)
}

/// Away from macOS no global click monitor is available; the ring keeps its
/// in-window dismissal paths (center ×, slot activation, timeout).
#[cfg(not(target_os = "macos"))]
pub struct ClickAwayMonitor(());

#[cfg(not(target_os = "macos"))]
pub fn watch_clicks_outside(_on_mouse_down: impl Fn() + 'static) -> Option<ClickAwayMonitor> {
    None
}

/// One display's global geometry, in the same top-left-origin global point
/// space that `openlogi_hook::cursor_position()` reports.
#[cfg(not(target_os = "windows"))]
pub struct CursorDisplay {
    /// Native display id; on macOS the `CGDirectDisplayID`, numerically equal
    /// to GPUI's `DisplayId` for the same display.
    pub id: u64,
    /// Global origin (top-left corner) of the display, in points.
    pub origin: (f64, f64),
    /// Display size in points.
    pub size: (f64, f64),
}

/// Find the display whose global bounds contain the point `(x, y)`.
///
/// GPUI's `PlatformDisplay::bounds()` zeroes every display's origin (window
/// bounds are display-relative), so mapping a global cursor position to its
/// display has to go through CoreGraphics.
#[cfg(target_os = "macos")]
#[expect(
    unsafe_code,
    reason = "CGGetActiveDisplayList/CGDisplayBounds are plain C FFI; GPUI exposes no global display bounds"
)]
pub fn display_containing(x: f64, y: f64) -> Option<CursorDisplay> {
    use core_graphics::display::{CGDisplayBounds, CGGetActiveDisplayList};

    const MAX_DISPLAYS: u32 = 32;
    let mut ids = [0u32; MAX_DISPLAYS as usize];
    let mut count = 0u32;
    // SAFETY: the list write is bounded by the capacity we pass; `count`
    // reports how many entries were actually filled.
    let result = unsafe { CGGetActiveDisplayList(MAX_DISPLAYS, ids.as_mut_ptr(), &raw mut count) };
    if result != 0 {
        return None;
    }
    ids.iter().take(count as usize).find_map(|&id| {
        // SAFETY: side-effect-free C getter on an id from the active list.
        let bounds = unsafe { CGDisplayBounds(id) };
        let contains = x >= bounds.origin.x
            && x < bounds.origin.x + bounds.size.width
            && y >= bounds.origin.y
            && y < bounds.origin.y + bounds.size.height;
        contains.then(|| CursorDisplay {
            id: u64::from(id),
            origin: (bounds.origin.x, bounds.origin.y),
            size: (bounds.size.width, bounds.size.height),
        })
    })
}

/// On Linux the GPUI display list already carries global origins, so there is
/// nothing to resolve natively.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn display_containing(_x: f64, _y: f64) -> Option<CursorDisplay> {
    None
}

/// The departing ring is decoration only; let clicks reach the application below.
#[cfg(target_os = "windows")]
pub(crate) fn release_pointer(window: &gpui::Window) {
    if let Err(error) = windows::release_pointer(window) {
        tracing::warn!(?error, "could not release Actions Ring pointer input");
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn release_pointer(_window: &gpui::Window) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    if let Some(marker) = MainThreadMarker::new() {
        for window in NSApplication::sharedApplication(marker).windows() {
            window.setIgnoresMouseEvents(true);
        }
    }
}

/// Wayland honours an empty input region; X11 has no GPUI equivalent, so the
/// call reaches `PlatformWindow`'s default no-op and the departing ring keeps
/// swallowing clicks there until it is destroyed.
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub(crate) fn release_pointer(window: &gpui::Window) {
    window.set_input_region(Some(&[]));
}

/// Whether outside-click dismissal is the agent's job on this platform.
///
/// Dismissing the ring on a click beside it has exactly one owner per platform.
/// macOS owns it natively in [`watch_clicks_outside`]: a global `NSEvent`
/// monitor sees the click without consuming it, so arming the agent as well
/// would make one click cancel the session twice, from two owners that cannot
/// see each other. Linux cannot own it either way — the evdev backend reports
/// no compositor coordinates ([`openlogi_hook::MouseEvent::Button::position`]
/// is `None` there), so the agent has nothing to hit-test a click against.
///
/// Windows has neither a global monitor nor a native dismissal path, which is
/// why the input region exists: the agent's hook already sees every click in
/// screen coordinates, and the published region is what makes that click
/// attributable to a ring session.
///
/// This gates only the *lease* — the region value itself is still recorded on
/// every platform, because the cursor warp is expressed against its origin.
pub(crate) const fn publishes_input_region() -> bool {
    cfg!(target_os = "windows")
}

/// Hide before destruction so native hover/activation notifications can run
/// while GPUI still owns the window. Never call native visibility APIs while
/// an App or Window update is on the stack.
pub(crate) fn close_window(window: &mut gpui::Window, cx: &mut gpui::App) {
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        #[cfg(target_os = "windows")]
        let _ = windows::hide(handle, cx);
        cx.spawn(async move |cx| {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        })
        .detach();
    })
    .detach();
}

/// Undo the Windows system's lines-per-notch setting before discrete ring selection.
#[cfg(target_os = "windows")]
#[expect(
    unsafe_code,
    reason = "read-only system scroll setting with a writable u32 output"
)]
pub(crate) fn normalize_ring_scroll(delta: gpui::ScrollDelta) -> gpui::ScrollDelta {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SPI_GETWHEELSCROLLLINES, SystemParametersInfoW,
    };
    let gpui::ScrollDelta::Lines(mut lines) = delta else {
        return delta;
    };
    let mut amount: u32 = 0;
    // SAFETY: the API writes exactly one u32 for SPI_GETWHEELSCROLLLINES.
    if unsafe { SystemParametersInfoW(SPI_GETWHEELSCROLLLINES, 0, (&raw mut amount).cast(), 0) }
        != 0
        && amount > 0
    {
        // The GPUI backend multiplies native notch distance by this same value,
        // including the Windows page-scroll sentinel.
        #[expect(
            clippy::cast_precision_loss,
            reason = "undo GPUI's identical u32-to-f32 wheel scaling"
        )]
        {
            lines.y /= amount as f32;
        }
    } else {
        lines.y = lines.y.signum();
    }
    gpui::ScrollDelta::Lines(lines)
}

#[cfg(not(target_os = "windows"))]
pub(crate) const fn normalize_ring_scroll(delta: gpui::ScrollDelta) -> gpui::ScrollDelta {
    delta
}

/// Ownership of the hidden system cursor for one ring session.
///
/// Both platforms expose *counted* hide APIs (Win32 `ShowCursor`, CoreGraphics
/// `CGDisplayHideCursor`), so an unbalanced hide leaves the user with no cursor
/// at all. Restoration is therefore tied to this value's lifetime instead of to
/// a caller remembering to undo it: a dropped session restores the cursor even
/// on the early-return and panic paths.
///
/// GPUI has no element-level or window-level equivalent. `CursorStyle` has no
/// hidden variant, and `Platform::hide_cursor_until_mouse_moves` is crate-private
/// and keyboard-triggered, so the ring owns this natively.
#[derive(Debug)]
pub(crate) struct HiddenCursor(());

impl HiddenCursor {
    /// Hide the system cursor until the returned value is dropped.
    pub(crate) fn acquire() -> Option<Self> {
        hide_system_cursor().then_some(Self(()))
    }
}

impl Drop for HiddenCursor {
    fn drop(&mut self) {
        show_system_cursor();
    }
}

/// `ShowCursor` keeps an internal display counter rather than a boolean, and its
/// starting value is not guaranteed, so drive it to the first negative value
/// instead of decrementing once. The bound stops a pathological counter from
/// spinning; a cursor we failed to hide is reported as not hidden, so no
/// unbalanced restore is queued for it.
#[cfg(target_os = "windows")]
#[expect(
    unsafe_code,
    reason = "Win32 ShowCursor takes no pointer and returns the new display count"
)]
fn hide_system_cursor() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::ShowCursor;
    for _ in 0..MAX_CURSOR_COUNTER_STEPS {
        // SAFETY: no arguments to validate and no output buffer; the call only
        // moves this thread's cursor display counter.
        if unsafe { ShowCursor(0) } < 0 {
            return true;
        }
    }
    tracing::warn!("could not hide the cursor for the Actions Ring");
    false
}

/// The exact inverse of [`hide_system_cursor`]: raise the counter back to the
/// visible range without overshooting into another component's hide.
#[cfg(target_os = "windows")]
#[expect(
    unsafe_code,
    reason = "Win32 ShowCursor takes no pointer and returns the new display count"
)]
fn show_system_cursor() {
    use windows_sys::Win32::UI::WindowsAndMessaging::ShowCursor;
    for _ in 0..MAX_CURSOR_COUNTER_STEPS {
        // SAFETY: as above; this direction restores what `acquire` consumed.
        if unsafe { ShowCursor(1) } >= 0 {
            return;
        }
    }
    tracing::warn!("could not restore the cursor after the Actions Ring");
}

/// Hide on the main display: the counted CoreGraphics call is process-wide for
/// cursor visibility, and the ring is a single cursor-centred session.
#[cfg(target_os = "macos")]
fn hide_system_cursor() -> bool {
    match core_graphics::display::CGDisplay::main().hide_cursor() {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(?error, "could not hide the cursor for the Actions Ring");
            false
        }
    }
}

#[cfg(target_os = "macos")]
fn show_system_cursor() {
    if let Err(error) = core_graphics::display::CGDisplay::main().show_cursor() {
        tracing::warn!(
            ?error,
            "could not restore the cursor after the Actions Ring"
        );
    }
}

/// Linux has no equivalent that works for a click-through popup the compositor
/// may not have given pointer focus; the ring keeps the system cursor there.
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const fn hide_system_cursor() -> bool {
    false
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const fn show_system_cursor() {}

#[cfg(any(target_os = "windows", target_os = "macos"))]
const MAX_CURSOR_COUNTER_STEPS: u8 = 64;

/// Move the system cursor to a global screen position.
///
/// The coordinate space is the one [`openlogi_core::action_ring::InputRegion`]
/// is published in: physical screen pixels on Windows, global points on macOS.
/// That region is the ring window's own rectangle, so it is the single origin
/// both the hit testing and this warp are expressed against.
#[cfg(target_os = "windows")]
pub(crate) fn warp_cursor(x: f64, y: f64) {
    if let Err(error) = windows::warp_cursor(x, y) {
        tracing::warn!(?error, "could not move the cursor to the selected slot");
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn warp_cursor(x: f64, y: f64) {
    use core_graphics::display::CGDisplay;
    use core_graphics::geometry::CGPoint;
    // Warping does not synthesize a move event, but AppKit still delivers the
    // resulting position change; the view filters that echo by position.
    if let Err(error) = CGDisplay::warp_mouse_cursor_position(CGPoint::new(x, y)) {
        tracing::warn!(?error, "could not move the cursor to the selected slot");
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub(crate) const fn warp_cursor(_x: f64, _y: f64) {}
