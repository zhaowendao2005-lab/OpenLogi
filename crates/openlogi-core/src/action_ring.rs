//! Shared Actions Ring timing and native input-region vocabulary.
//!
//! The ring's runtime session state (`ActionRingManager`: `Mutex`, `Notify`,
//! `Instant`) is agent-only and stays in `openlogi-agent-core` — it is not a
//! shared contract. This constant is, though: the agent derives the
//! session's expiry window from it and the GUI's overlay helper times its own
//! window by it, so the two clocks cannot drift out of step. The ring's
//! persisted layout/config schema ([`crate::binding::ActionRingLayout`] and
//! friends) is a separate concern, unrelated to this runtime timing value.

use std::time::Duration;

/// How long the overlay keeps the ring on screen, counted from the moment its
/// window opens. The overlay owns the display; the constant lives here so the
/// session that has to outlive it is derived from it rather than kept in step
/// by hand.
pub const DISPLAY_LIFETIME: Duration = Duration::from_secs(15);

/// A short renewable grant, so an unresponsive overlay cannot retain input interception.
pub const INPUT_LEASE: Duration = Duration::from_secs(2);
/// Renewal cadence owned alongside the lease.
pub const INPUT_RENEWAL: Duration = Duration::from_millis(500);

/// Native event-coordinate rectangle (physical pixels on Windows, points on macOS).
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct InputRegion {
    /// Left edge in the native event coordinate space.
    pub x: f64,
    /// Top edge in the native event coordinate space.
    pub y: f64,
    /// Width in native event units.
    pub width: f64,
    /// Height in native event units.
    pub height: f64,
}
impl InputRegion {
    /// Reject malformed geometry rather than intercept unrelated clicks.
    #[must_use]
    pub fn is_valid(self) -> bool {
        [self.x, self.y, self.width, self.height]
            .into_iter()
            .all(f64::is_finite)
            && self.width > 0.0
            && self.height > 0.0
            && (self.x + self.width).is_finite()
            && (self.y + self.height).is_finite()
    }
    /// Whether a native pointer location is inside this rectangle.
    #[must_use]
    pub fn contains(self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}
