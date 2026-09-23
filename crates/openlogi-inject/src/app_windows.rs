//! Native application-window inventory and activation. No GUI or IPC dependencies.

#[cfg(target_os = "windows")]
mod windows;

/// A native window retained only for the lifetime of an agent session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WindowTarget {
    #[cfg(target_os = "windows")]
    window: crate::ForegroundWindow,
}

impl WindowTarget {
    /// Capture the window at physical button-down, before an overlay opens.
    #[must_use]
    pub fn capture() -> Option<Self> {
        #[cfg(target_os = "windows")]
        {
            crate::ForegroundWindow::capture().map(|window| Self { window })
        }
        #[cfg(not(target_os = "windows"))]
        {
            None
        }
    }

    /// Reuse the foreground snapshot carried by the action lifecycle. This is
    /// the only supported way to turn a trigger target into a window target:
    /// calling [`Self::capture`] after the overlay exists would capture the
    /// overlay instead of the application the user aimed at.
    #[cfg(target_os = "windows")]
    #[must_use]
    pub fn from_foreground(window: crate::ForegroundWindow) -> Self {
        Self { window }
    }

    /// A process/window identity suitable for in-process history keys.
    #[must_use]
    pub fn identity(self) -> (usize, u32) {
        #[cfg(target_os = "windows")]
        {
            self.window.identity()
        }
        #[cfg(not(target_os = "windows"))]
        {
            (0, 0)
        }
    }

    /// Whether this target belongs to OpenLogi's own overlay helper.
    #[must_use]
    pub fn is_openlogi_overlay(self) -> bool {
        #[cfg(target_os = "windows")]
        {
            self.window.is_openlogi_overlay()
        }
        #[cfg(not(target_os = "windows"))]
        {
            false
        }
    }

    /// Activate exactly this window without making it permanently topmost.
    #[must_use]
    pub fn activate(self) -> bool {
        #[cfg(target_os = "windows")]
        {
            self.window.activate()
        }
        #[cfg(not(target_os = "windows"))]
        {
            false
        }
    }
}

/// One ordinary, visible top-level application window (including minimized windows).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppWindow {
    /// In-process activation capability; never sent to an IPC client.
    pub target: WindowTarget,
    /// Executable identity used to group access history. It is never sent to
    /// an IPC client and is not persisted.
    pub executable: String,
    /// Native title, used only for display, not identity.
    pub title: String,
    /// Whether the window was minimized when it was enumerated. Activation
    /// restores it before requesting the foreground.
    pub minimized: bool,
}

/// Enumerate windows belonging to the captured application's executable.
/// Unsupported platforms explicitly report an error rather than synthesizing Alt+Tab.
pub fn list(target: WindowTarget) -> std::io::Result<Vec<AppWindow>> {
    #[cfg(target_os = "windows")]
    {
        windows::list(target)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = target;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "application window switching is not supported on this platform",
        ))
    }
}
