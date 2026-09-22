//! OS input-event synthesis split out of openlogi-core so the core stays platform- and IO-free.

mod inject;

pub use inject::{
    HeldChord, SYNTHETIC_EVENT_USER_DATA, SmoothScrollPhase, ax_navigate_browser, execute,
    post_scroll, post_smooth_scroll, press_hold,
};

#[cfg(target_os = "linux")]
pub use inject::action_device_path;

/// The Windows-only target snapshot: synthetic input there is delivered to
/// whatever holds focus, so a caller whose own UI took the foreground has to
/// say which window the action was actually aimed at.
#[cfg(target_os = "windows")]
pub use inject::ForegroundWindow;
