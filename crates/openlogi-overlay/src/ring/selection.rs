//! Wheel selection owns its fractional input and pointer-mode handoff.
use gpui::ScrollDelta;
use openlogi_core::binding::ActionRingSlot;

#[derive(Default)]
pub(super) struct WheelSelection {
    active: bool,
    remainder: f32,
}
impl WheelSelection {
    pub fn active(&self) -> bool {
        self.active
    }
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    pub fn scroll(
        &mut self,
        delta: ScrollDelta,
        current: Option<ActionRingSlot>,
        enabled: [bool; 8],
    ) -> Option<ActionRingSlot> {
        let ticks = match delta {
            ScrollDelta::Lines(delta) => -delta.y,
            ScrollDelta::Pixels(delta) => -f32::from(delta.y) / 40.0,
        };
        if !ticks.is_finite() || ticks == 0.0 || !enabled.iter().any(|v| *v) {
            return None;
        }
        if self.remainder.signum() != ticks.signum() {
            self.remainder = 0.0;
        }
        self.remainder += ticks;
        let whole = self.remainder.trunc();
        self.remainder -= whole;
        if whole == 0.0 {
            return None;
        }
        self.active = true;
        let forward = whole > 0.0;
        let slots: Vec<_> = ActionRingSlot::ALL
            .into_iter()
            .filter(|s| enabled[s.index()])
            .collect();
        let start = current.and_then(|s| slots.iter().position(|v| *v == s));
        // At most seven extra steps matter; discard complete turns, not fractional input.
        let mut remaining = whole.abs();
        let mut index = start.unwrap_or_else(|| if forward { slots.len() - 1 } else { 0 });
        remaining %= f32::from(u8::try_from(slots.len()).unwrap_or(8));
        while remaining >= 1.0 {
            index = if forward {
                (index + 1) % slots.len()
            } else {
                (index + slots.len() - 1) % slots.len()
            };
            remaining -= 1.0;
        }
        Some(slots[index])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px};
    #[test]
    fn wheel_wraps_skips_empty_and_resets() {
        let mut wheel = WheelSelection::default();
        let enabled = [true, false, true, false, false, false, false, false];
        let down = ScrollDelta::Lines(point(0.0, -1.0));
        assert_eq!(wheel.scroll(down, None, enabled), Some(ActionRingSlot::Top));
        assert_eq!(
            wheel.scroll(down, Some(ActionRingSlot::Top), enabled),
            Some(ActionRingSlot::Right)
        );
        assert_eq!(
            wheel.scroll(down, Some(ActionRingSlot::Right), enabled),
            Some(ActionRingSlot::Top)
        );
        assert!(wheel.active());
        wheel.reset();
        assert!(!wheel.active());
        assert_eq!(wheel.scroll(down, None, [false; 8]), None);
    }
    #[test]
    fn pixels_accumulate_and_reverse_without_stale_remainder() {
        let mut wheel = WheelSelection::default();
        let delta = |y| ScrollDelta::Pixels(point(px(0.0), px(y)));
        assert_eq!(wheel.scroll(delta(-20.0), None, [true; 8]), None);
        assert_eq!(
            wheel.scroll(delta(-20.0), None, [true; 8]),
            Some(ActionRingSlot::Top)
        );
        assert_eq!(
            wheel.scroll(delta(40.0), Some(ActionRingSlot::Top), [true; 8]),
            Some(ActionRingSlot::TopLeft)
        );
    }
}
