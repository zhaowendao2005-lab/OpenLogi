//! View-owned, finite visual transitions. No session deadlines or commands.

use std::time::{Duration, Instant};

use openlogi_core::binding::ActionRingSlot;

use super::geometry::{Pose, shortest_turn, slot_angle};

#[derive(Clone, Copy)]
struct Tween {
    from: f32,
    to: f32,
    started: Instant,
    duration: Duration,
}

impl Tween {
    fn new(value: f32, now: Instant, millis: u64) -> Self {
        Self {
            from: value,
            to: value,
            started: now,
            duration: Duration::from_millis(millis),
        }
    }

    fn sample(self, now: Instant) -> f32 {
        self.from
            + (self.to - self.from)
                * ease(progress(
                    now.saturating_duration_since(self.started),
                    self.duration,
                ))
    }

    fn retarget(&mut self, to: f32, now: Instant) {
        self.from = self.sample(now);
        self.to = to;
        self.started = now;
    }

    fn active(self, now: Instant) -> bool {
        (self.from - self.to).abs() > f32::EPSILON
            && now.saturating_duration_since(self.started) < self.duration
    }
}

#[derive(Clone, Copy)]
pub(super) struct SectorFrame {
    pub pose: Pose,
    pub highlight: f32,
    pub content_scale: f32,
}

#[derive(Clone, Copy)]
pub(super) struct Frame {
    pub sectors: [SectorFrame; 8],
    pub pointer_angle: f32,
    pub pointer_opacity: f32,
    pub guides_opacity: f32,
    pub active: bool,
}

pub(super) struct Motion {
    opened: Instant,
    highlight: [Tween; 8],
    content: [Tween; 8],
    pointer: Tween,
    visibility: Tween,
}

impl Motion {
    pub fn new(now: Instant) -> Self {
        Self {
            opened: now,
            highlight: [Tween::new(0.0, now, 110); 8],
            content: [Tween::new(0.0, now, 180); 8],
            pointer: Tween::new(0.0, now, 160),
            visibility: Tween::new(0.0, now, 100),
        }
    }

    pub fn select(&mut self, selected: Option<ActionRingSlot>, now: Instant) {
        for slot in ActionRingSlot::ALL {
            let target = if selected == Some(slot) { 1.0 } else { 0.0 };
            self.highlight[slot.index()].retarget(target, now);
            self.content[slot.index()].retarget(target, now);
        }
        if let Some(slot) = selected {
            let current = self.pointer.sample(now);
            self.pointer
                .retarget(current + shortest_turn(current, slot_angle(slot)), now);
        }
        self.visibility
            .retarget(if selected.is_some() { 1.0 } else { 0.0 }, now);
    }

    pub fn frame(&self, now: Instant, reduce_motion: bool) -> Frame {
        let elapsed = now.saturating_duration_since(self.opened);
        let value = |tween: Tween| {
            if reduce_motion {
                tween.to
            } else {
                tween.sample(now)
            }
        };
        let sectors = ActionRingSlot::ALL.map(|slot| {
            let deploy = if reduce_motion {
                1.0
            } else {
                let delay = Duration::from_secs_f32(slot_angle(slot) / 45.0 * 0.022);
                ease(progress(
                    elapsed.saturating_sub(delay),
                    Duration::from_millis(410),
                ))
            };
            SectorFrame {
                pose: Pose {
                    scale: 0.67 + 0.33 * deploy,
                    rotation: -9.0 * (1.0 - deploy),
                    opacity: deploy,
                },
                highlight: value(self.highlight[slot.index()]),
                content_scale: 1.0 + 0.055 * value(self.content[slot.index()]),
            }
        });
        Frame {
            sectors,
            pointer_angle: value(self.pointer),
            pointer_opacity: value(self.visibility),
            guides_opacity: if reduce_motion {
                1.0
            } else {
                ease(progress(elapsed, Duration::from_millis(650)))
            },
            active: !reduce_motion
                && (elapsed < Duration::from_millis(650)
                    || self
                        .highlight
                        .iter()
                        .chain(&self.content)
                        .any(|tween| tween.active(now))
                    || self.pointer.active(now)
                    || self.visibility.active(now)),
        }
    }
}

fn progress(elapsed: Duration, duration: Duration) -> f32 {
    (elapsed.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0)
}

/// The reference's cubic-bezier(.16, 1, .3, 1), inverted on its time axis.
fn ease(progress: f32) -> f32 {
    if progress <= 0.0 || progress >= 1.0 {
        return progress;
    }
    let (mut low, mut high) = (0.0_f32, 1.0_f32);
    for _ in 0..14 {
        let t = low.midpoint(high);
        let x = 3.0 * (1.0 - t).powi(2) * t * 0.16 + 3.0 * (1.0 - t) * t * t * 0.3 + t * t * t;
        if x < progress {
            low = t;
        } else {
            high = t;
        }
    }
    1.0 - (1.0 - low.midpoint(high)).powi(3)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deployment_is_staggered_and_eventually_stops() {
        let now = Instant::now();
        let motion = Motion::new(now);
        let early = motion.frame(now + Duration::from_millis(80), false);
        assert!(early.sectors[0].pose.opacity > early.sectors[3].pose.opacity);
        assert!(early.sectors[7].pose.opacity.abs() < f32::EPSILON);
        let settled = motion.frame(now + Duration::from_secs(1), false);
        assert!(!settled.active);
        assert!(
            settled
                .sectors
                .iter()
                .all(|sector| (sector.pose.scale - 1.0).abs() < f32::EPSILON)
        );
    }

    #[test]
    fn pointer_takes_shortest_turn_and_retargets_without_jumping() {
        let now = Instant::now();
        let mut motion = Motion::new(now);
        motion.select(Some(ActionRingSlot::TopLeft), now);
        let later = now + Duration::from_millis(40);
        let angle = motion.frame(later, false).pointer_angle;
        assert!(angle < 0.0 && angle > -45.0);
        motion.select(Some(ActionRingSlot::TopRight), later);
        assert!((motion.frame(later, false).pointer_angle - angle).abs() < 0.001);
        assert!(
            (motion
                .frame(later + Duration::from_secs(1), false)
                .pointer_angle
                - 45.0)
                .abs()
                < 0.001
        );
    }

    #[test]
    fn reduced_motion_is_static_and_selection_remains_visible() {
        let now = Instant::now();
        let mut motion = Motion::new(now);
        motion.select(Some(ActionRingSlot::Right), now);
        let frame = motion.frame(now, true);
        assert!(!frame.active);
        assert!((frame.pointer_angle - 90.0).abs() < 0.001);
        assert!((frame.sectors[2].highlight - 1.0).abs() < f32::EPSILON);
        assert!(
            frame
                .sectors
                .iter()
                .all(|sector| (sector.pose.opacity - 1.0).abs() < f32::EPSILON)
        );
        motion.select(None, now);
        assert!(motion.frame(now, true).pointer_opacity.abs() < f32::EPSILON);
    }
}
