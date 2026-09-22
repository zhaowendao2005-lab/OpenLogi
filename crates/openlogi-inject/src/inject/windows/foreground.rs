//! The window an action must be aimed at, captured with its physical trigger.
//!
//! `SendInput` has no window parameter: it feeds the system input queue, which
//! delivers to whatever holds focus *at injection time*. The Actions Ring
//! overlay is an ordinary top-level window, so putting it up takes the
//! foreground away from the application the user triggered it over, and it
//! still holds the foreground while its exit animation plays. A shortcut
//! injected then lands in the closing overlay and is lost.
//!
//! Capturing the foreground window with the trigger and restoring it
//! immediately before injection decouples the two: the overlay keeps its exit
//! animation, and the action still reaches the window the user aimed at.
#![expect(
    unsafe_code,
    reason = "foreground restoration is the Win32 window/input-queue C API"
)]

use std::fmt;

use windows_sys::Win32::Foundation::{GetLastError, HWND};
use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, IsWindow, IsWindowVisible,
    SetForegroundWindow,
};

/// The foreground window at the instant a physical trigger arrived.
///
/// Identity is the handle *plus* its owning process: Windows recycles `HWND`
/// values, so a handle that is live again by the time the action runs may
/// belong to an entirely different application. The handle is held as its
/// address rather than as a pointer so the snapshot stays `Send` and can ride
/// the trigger through the agent's asynchronous dispatch.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ForegroundWindow {
    hwnd: usize,
    thread: u32,
    process: u32,
}

impl fmt::Debug for ForegroundWindow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ForegroundWindow")
            .field("hwnd", &format_args!("{:#x}", self.hwnd))
            .field("thread", &self.thread)
            .field("process", &self.process)
            .finish()
    }
}

impl ForegroundWindow {
    /// Snapshot the window that currently holds the foreground, or `None` when
    /// there is none to aim at (an empty desktop, or a window whose owner the
    /// system will not name).
    #[must_use]
    pub fn capture() -> Option<Self> {
        // SAFETY: GetForegroundWindow takes no arguments and returns a window
        // handle or null; it has no preconditions.
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.is_null() {
            return None;
        }
        let (thread, process) = owner(hwnd)?;
        Some(Self {
            hwnd: hwnd.addr(),
            thread,
            process,
        })
    }

    /// Put this window back in front, and report whether it holds the
    /// foreground when the call returns.
    ///
    /// `false` means the action is about to land somewhere else — the window
    /// closed, its handle was recycled by another process, or the shell
    /// refused the foreground change. Callers inject anyway rather than
    /// dropping the action: reaching the wrong window is bad, reaching nothing
    /// at all is worse.
    pub fn restore(self) -> bool {
        let hwnd = self.handle();
        // SAFETY: GetForegroundWindow takes no arguments and has no preconditions.
        let current = unsafe { GetForegroundWindow() };
        if current == hwnd {
            // The common case for a directly bound button: nothing ever took
            // the foreground away, so there is nothing to restore.
            return true;
        }
        // SAFETY: both calls accept any handle value by value and answer
        // whether it still names a live, visible window.
        if unsafe { IsWindow(hwnd) } == 0 || unsafe { IsWindowVisible(hwnd) } == 0 {
            tracing::warn!(
                target_window = ?self,
                foreground_hwnd = current.addr(),
                "the trigger-time window is gone — injecting into the current foreground instead"
            );
            return false;
        }
        let Some((thread, process)) = owner(hwnd) else {
            tracing::warn!(
                target_window = ?self,
                api = "GetWindowThreadProcessId",
                "the trigger-time window no longer names an owner — injecting into the current foreground instead"
            );
            return false;
        };
        if process != self.process {
            tracing::warn!(
                target_window = ?self,
                process,
                "the trigger-time window handle was recycled by another process — injecting into the current foreground instead"
            );
            return false;
        }

        tracing::debug!(
            target_window = ?self,
            foreground_hwnd = current.addr(),
            "restoring the trigger-time foreground window before injection"
        );
        // SAFETY: GetCurrentThreadId takes no arguments and has no preconditions.
        let ours = unsafe { GetCurrentThreadId() };
        // Windows refuses a foreground change from a process that is neither
        // the foreground process nor the one that received the last input
        // event — and this agent is neither: the Actions Ring overlay is a
        // separate process and it is what holds the foreground. Joining the
        // *current* foreground thread's input queue is the documented way to
        // earn the right; joining the target's is what makes `SetFocus` land
        // inside it rather than on this thread's own (empty) focus.
        //
        // `AttachThreadInput` is not reference counted, so each thread is
        // joined at most once and every join is undone exactly once below.
        let mut attached = Vec::with_capacity(2);
        for theirs in [owner(current).map(|(thread, _)| thread), Some(thread)]
            .into_iter()
            .flatten()
        {
            if theirs != ours && !attached.contains(&theirs) && attach(ours, theirs, true) {
                attached.push(theirs);
            }
        }
        // SAFETY: `hwnd` names the live window checked above and is passed by
        // value; the returned status is only read for logging.
        let set = unsafe { SetForegroundWindow(hwnd) };
        // SAFETY: as above.
        unsafe { BringWindowToTop(hwnd) };
        // SAFETY: as above; SetFocus returns the previously focused window.
        unsafe { SetFocus(hwnd) };
        // Detach before injecting: the shared input state was only needed to
        // make the foreground change legal, and leaving the queues joined
        // across the injection would make the target's input state depend on
        // this agent's.
        for theirs in attached {
            attach(ours, theirs, false);
        }

        // SAFETY: GetForegroundWindow takes no arguments and has no preconditions.
        let restored = unsafe { GetForegroundWindow() } == hwnd;
        if restored {
            tracing::debug!(target_window = ?self, foreground_restored = true, "foreground restored");
        } else {
            // SAFETY: GetLastError reads the calling thread's last-error code
            // and has no preconditions.
            let error = unsafe { GetLastError() };
            tracing::warn!(
                target_window = ?self,
                foreground_restored = false,
                api = "SetForegroundWindow",
                result = set,
                error,
                "could not restore the trigger-time foreground window — the action will land in the current foreground"
            );
        }
        restored
    }

    /// The captured handle. Rebuilt without provenance because an `HWND` is an
    /// opaque system handle, never a pointer this process may dereference.
    fn handle(self) -> HWND {
        std::ptr::without_provenance_mut(self.hwnd)
    }
}

/// The thread and process that own `hwnd`, or `None` when the system declines
/// to name either — which is how a destroyed window answers.
fn owner(hwnd: HWND) -> Option<(u32, u32)> {
    let mut process = 0;
    // SAFETY: `hwnd` is passed by value and `&raw mut process` is a valid
    // out-pointer the call writes the owning process id into.
    let thread = unsafe { GetWindowThreadProcessId(hwnd, &raw mut process) };
    (thread != 0 && process != 0).then_some((thread, process))
}

/// Join or leave `theirs`'s input queue, reporting whether the system agreed.
fn attach(ours: u32, theirs: u32, join: bool) -> bool {
    // SAFETY: AttachThreadInput takes two thread ids and a flag by value.
    let attached = unsafe { AttachThreadInput(ours, theirs, i32::from(join)) } != 0;
    if !attached {
        // SAFETY: GetLastError reads the calling thread's last-error code and
        // has no preconditions.
        let error = unsafe { GetLastError() };
        tracing::debug!(
            api = "AttachThreadInput",
            join,
            error,
            ours,
            theirs,
            "could not join the target window's input queue"
        );
    }
    attached
}

#[cfg(test)]
mod tests {
    use super::ForegroundWindow;

    /// A snapshot whose window is gone must report the failure rather than
    /// claim the injection is aimed correctly — the caller logs that the
    /// action is about to land in whatever holds the foreground instead.
    #[test]
    fn a_dead_window_handle_never_claims_the_foreground() {
        let dead = ForegroundWindow {
            hwnd: 1,
            thread: 1,
            process: 1,
        };
        assert!(!dead.restore());
    }
}
