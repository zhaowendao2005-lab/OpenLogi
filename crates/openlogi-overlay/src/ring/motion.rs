//! View-owned, finite visual transitions. No session deadlines or commands.

use std::time::{Duration, Instant};

use openlogi_core::binding::ActionRingSlot;

use super::geometry::{Pose, shortest_turn, slot_angle};

const OPEN_DURATION: Duration = Duration::from_millis(180);
const OPEN_FADE_DURATION: Duration = Duration::from_millis(90);

const CLOSE_DURATION: Duration = Duration::from_millis(150);

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
    pub guides: Pose,
    pub core_opacity: f32,
    pub closed: bool,
    pub active: bool,
}

pub(super) struct Motion {
    speed: openlogi_core::binding::ActionRingAnimationSpeed,
    opened: Instant,
    closing: Option<(Instant, Frame)>,
    highlight: [Tween; 8],
    content: [Tween; 8],
    pointer: Tween,
    visibility: Tween,
}

impl Motion {
    pub fn new(now: Instant, speed: openlogi_core::binding::ActionRingAnimationSpeed) -> Self {
        Self {
            speed,
            opened: now,
            closing: None,
            highlight: [Tween::new(0.0, now, 110); 8],
            content: [Tween::new(0.0, now, 180); 8],
            pointer: Tween::new(0.0, now, 160),
            visibility: Tween::new(0.0, now, 100),
        }
    }

    pub fn before_show(&self, reduce_motion: bool) -> Frame {
        self.frame(self.opened, reduce_motion)
    }

    pub fn is_closing(&self) -> bool {
        self.closing.is_some()
    }

    pub fn close(&mut self, now: Instant, reduce_motion: bool) -> Duration {
        if self.closing.is_none() {
            self.closing = Some((now, self.frame(now, reduce_motion)));
        }
        if reduce_motion {
            Duration::ZERO
        } else {
            self.speed.duration(CLOSE_DURATION)
        }
    }
    pub fn select(&mut self, selected: Option<ActionRingSlot>, now: Instant) {
        if self.is_closing() {
            return;
        }
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
        if let Some((started, snapshot)) = self.closing {
            let elapsed = now.saturating_duration_since(started);
            let amount = if reduce_motion {
                1.0
            } else {
                progress(elapsed, self.speed.duration(CLOSE_DURATION))
            };
            let fold = smoothstep(amount);
            let retract = |pose: Pose| Pose {
                scale: pose.scale * (1.0 - 0.66 * fold),
                rotation: pose.rotation - 62.0 * fold,
                opacity: pose.opacity * (1.0 - fold),
            };
            return Frame {
                sectors: snapshot.sectors.map(|sector| SectorFrame {
                    pose: retract(sector.pose),
                    ..sector
                }),
                guides: retract(snapshot.guides),
                core_opacity: snapshot.core_opacity
                    * (1.0 - ((amount - 0.6) / 0.4).clamp(0.0, 1.0)),
                pointer_opacity: snapshot.pointer_opacity * (1.0 - fold),
                pointer_angle: snapshot.pointer_angle - 62.0 * fold,
                active: amount < 1.0,
                closed: amount >= 1.0,
            };
        }
        let elapsed = now
            .saturating_duration_since(self.opened)
            .mul_f32(self.speed.multiplier());
        let value = |tween: Tween| {
            if reduce_motion {
                tween.to
            } else {
                tween.sample(now)
            }
        };
        // One pose keeps the entire ring readable throughout its short entry.
        let deploy = if reduce_motion {
            1.0
        } else {
            smoothstep(progress(elapsed, OPEN_DURATION))
        };
        let opacity = if reduce_motion {
            1.0
        } else {
            smoothstep(progress(elapsed, OPEN_FADE_DURATION))
        };
        let pose = Pose {
            scale: 0.80 + 0.20 * deploy,
            rotation: -12.0 * (1.0 - deploy),
            opacity,
        };
        let sectors = ActionRingSlot::ALL.map(|slot| SectorFrame {
            pose,
            highlight: value(self.highlight[slot.index()]),
            content_scale: 1.0 + 0.055 * value(self.content[slot.index()]),
        });
        Frame {
            sectors,
            pointer_angle: value(self.pointer),
            pointer_opacity: value(self.visibility) * opacity,
            guides: pose,
            core_opacity: opacity,
            closed: false,
            active: !reduce_motion
                && (elapsed < OPEN_DURATION
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

fn smoothstep(amount: f32) -> f32 {
    amount * amount * (3.0 - 2.0 * amount)
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
    fn deployment_is_immediate_synchronized_and_stops_on_time() {
        let now = Instant::now();
        for percent in [50, 80, 100, 200] {
            let speed = openlogi_core::binding::ActionRingAnimationSpeed::try_new(percent).unwrap();
            let motion = Motion::new(now, speed);
            let first = motion.before_show(false);
            assert!((first.guides.scale - 0.80).abs() < 0.001);
            assert!((first.guides.rotation + 12.0).abs() < 0.001);
            assert!(first.core_opacity.abs() < f32::EPSILON);
            for millis in [1, 45, 90, 179, 180] {
                let frame =
                    motion.frame(now + speed.duration(Duration::from_millis(millis)), false);
                assert!(frame.core_opacity > 0.0);
                for sector in frame.sectors {
                    assert!((sector.pose.opacity - frame.core_opacity).abs() < 0.001);
                    assert!((sector.pose.scale - frame.guides.scale).abs() < 0.001);
                    assert!((sector.pose.rotation - frame.guides.rotation).abs() < 0.001);
                }
                assert!((frame.guides.opacity - frame.core_opacity).abs() < 0.001);
                assert_eq!(frame.active, millis < 180);
                if millis >= 90 {
                    assert!((frame.core_opacity - 1.0).abs() < 0.001);
                }
                if millis == 180 {
                    assert!((frame.guides.scale - 1.0).abs() < 0.001);
                    assert!(frame.guides.rotation.abs() < 0.001);
                }
            }
        }
    }

    #[test]
    fn pointer_takes_shortest_turn_and_retargets_without_jumping() {
        let now = Instant::now();
        let mut motion = Motion::new(
            now,
            openlogi_core::binding::ActionRingAnimationSpeed::try_new(100).unwrap(),
        );
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
        let mut motion = Motion::new(
            now,
            openlogi_core::binding::ActionRingAnimationSpeed::try_new(100).unwrap(),
        );
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

    #[test]
    fn synchronized_entry_and_exit_reverse_without_a_jump() {
        let now = Instant::now();
        let mut motion = Motion::new(
            now,
            openlogi_core::binding::ActionRingAnimationSpeed::try_new(100).unwrap(),
        );
        let first = motion.before_show(false);
        assert!(first.core_opacity.abs() < f32::EPSILON);
        assert!(
            first
                .sectors
                .iter()
                .all(|sector| sector.pose.opacity.abs() < f32::EPSILON)
        );
        assert!(
            motion
                .frame(now + Duration::from_millis(40), false)
                .guides
                .opacity
                .abs()
                > 0.0
        );
        let ready = now + Duration::from_millis(300);
        let before = motion.frame(ready, false);
        assert!(!before.active);
        let duration = motion.close(ready, false);
        let initial = motion.frame(ready, false);
        assert!((initial.sectors[0].pose.rotation - before.sectors[0].pose.rotation).abs() < 0.001);
        assert!((initial.sectors[0].pose.scale - before.sectors[0].pose.scale).abs() < 0.001);
        let middle = motion.frame(ready + duration / 2, false);
        assert!(middle.sectors[0].pose.rotation < before.sectors[0].pose.rotation);
        assert!(middle.sectors[0].pose.scale < before.sectors[0].pose.scale);
        assert!((middle.core_opacity - 1.0).abs() < f32::EPSILON);
        motion.close(ready + duration / 2, false);
        let closed = motion.frame(ready + duration, false);
        assert!(closed.closed && !closed.active);
        assert!(closed.core_opacity.abs() < f32::EPSILON);
    }

    #[test]
    fn interrupted_entry_and_reduced_motion_exit_are_bounded() {
        let now = Instant::now();
        for delay in [0, 30, 90, 200] {
            let mut motion = Motion::new(
                now,
                openlogi_core::binding::ActionRingAnimationSpeed::try_new(100).unwrap(),
            );
            let at = now + Duration::from_millis(delay);
            let before = motion.frame(at, false);
            let duration = motion.close(at, false);
            let after = motion.frame(at, false);
            for (after, before) in after.sectors.into_iter().zip(before.sectors) {
                assert!((after.pose.opacity - before.pose.opacity).abs() < 0.001);
                assert!((after.pose.scale - before.pose.scale).abs() < 0.001);
                assert!((after.pose.rotation - before.pose.rotation).abs() < 0.001);
            }
            assert!((after.core_opacity - before.core_opacity).abs() < 0.001);
            assert!(motion.frame(at + duration, false).closed);
        }
        let mut motion = Motion::new(
            now,
            openlogi_core::binding::ActionRingAnimationSpeed::try_new(100).unwrap(),
        );
        assert_eq!(motion.close(now, true), Duration::ZERO);
        assert!(motion.frame(now, true).closed);
    }
    #[test]
    fn playback_speed_scales_entry_and_exit_but_not_hover_feedback() {
        use openlogi_core::binding::ActionRingAnimationSpeed;
        let now = Instant::now();
        for percent in [50, 80, 100, 200] {
            let speed = ActionRingAnimationSpeed::try_new(percent).unwrap();
            let mut motion = Motion::new(now, speed);
            let entering = now + speed.duration(Duration::from_millis(100));
            assert!(motion.frame(entering, false).active);
            let ready = now + speed.duration(Duration::from_millis(300));
            assert!(!motion.frame(ready, false).active);
            motion.select(Some(ActionRingSlot::Top), ready);
            assert!(
                (motion
                    .frame(ready + Duration::from_millis(120), false)
                    .sectors[0]
                    .highlight
                    - 1.0)
                    .abs()
                    < 0.001
            );
            let at = ready + Duration::from_secs(1);
            let duration = motion.close(at, false);
            assert_eq!(duration, speed.duration(CLOSE_DURATION));
            assert!(!motion.frame(at + duration / 2, false).closed);
            assert!(motion.frame(at + duration, false).closed);
        }
    }
}
