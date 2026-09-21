//! Native vector presentation for the cursor-centred Actions Ring.
//! Session commands and window placement remain owned by their existing layers.

mod geometry;
mod masks;
mod motion;
mod paint;
mod selection;
mod timing;

use std::{cell::Cell, rc::Rc, sync::Arc, time::Instant};

use gpui::{
    Bounds, Context, InteractiveElement, IntoElement, ParentElement, Pixels, Point, Render,
    SharedString, StatefulInteractiveElement as _, Styled, Window, WindowBackgroundAppearance,
    WindowKind, WindowOptions, canvas, div, prelude::FluentBuilder as _, px,
};
#[cfg(any(not(target_os = "windows"), test))]
use gpui::{Size, point};
use openlogi_core::binding::{Action, ActionRingSlot};
use openlogi_ipc::{ActionRingInvocation, ActionRingPresentation};
use tokio::sync::mpsc;

use crate::ipc::OverlayCommand;
use crate::session::{ClickAwaySession, ShowingRing};
use geometry::{Geometry, PixelGrid, Pose};
use motion::{Frame, Motion};

pub(crate) const WINDOW_SIZE: f32 = 360.0;

pub(crate) struct RingView {
    region_publisher: Option<crate::ipc::RegionPublisher>,
    region_lease: Option<crate::ipc::RegionLease>,
    invocation: ActionRingInvocation,
    commands: mpsc::UnboundedSender<OverlayCommand>,
    hovered: Option<ActionRingSlot>,
    motion: Motion,
    presented: bool,
    frame_pending: bool,
    timing: timing::FrameTiming,
    wheel: selection::WheelSelection,
    ring_masks: Option<Rc<masks::RingMasks>>,
    pointer_position: Option<Point<Pixels>>,
    /// The window's own global screen rectangle, as published to the agent.
    /// It is the single origin the cursor warp is expressed against.
    input_region: Option<openlogi_core::action_ring::InputRegion>,
    /// The position this view warped the cursor to, until its echo arrives.
    warped_to: Option<Point<Pixels>>,
    /// Held while a slot is selected; restores the system cursor when dropped.
    hidden_cursor: Option<crate::platform::HiddenCursor>,
    /// Layout bookkeeping written by the canvas, not another selection owner.
    bounds: Rc<Cell<Bounds<Pixels>>>,
    /// Publishes click-away identity until input ends, before the visual exit.
    showing: Option<ShowingRing>,
}

impl RingView {
    /// Open a view on `invocation`, reporting interactions through `commands`.
    pub(crate) fn new(
        invocation: ActionRingInvocation,
        commands: mpsc::UnboundedSender<OverlayCommand>,
        live: &Arc<ClickAwaySession>,
    ) -> Self {
        let showing = live.showing(invocation.session_id);
        let speed = invocation.animation_speed;
        Self {
            region_publisher: None,
            region_lease: None,
            invocation,
            commands,
            hovered: None,
            motion: Motion::new(Instant::now(), speed),
            presented: false,
            frame_pending: false,
            timing: timing::FrameTiming::default(),
            wheel: selection::WheelSelection::default(),
            ring_masks: None,
            pointer_position: None,
            input_region: None,
            warped_to: None,
            hidden_cursor: None,
            bounds: Rc::new(Cell::new(Bounds::default())),
            showing: Some(showing),
        }
    }

    pub(crate) fn with_region_publisher(mut self, publisher: crate::ipc::RegionPublisher) -> Self {
        self.region_publisher = Some(publisher);
        self
    }

    pub(crate) fn publish_input_region(&mut self, region: openlogi_core::action_ring::InputRegion) {
        self.input_region = Some(region);
        if let Some(publisher) = &self.region_publisher {
            self.region_lease = Some(crate::ipc::RegionLease::new(
                publisher.clone(),
                self.session_id(),
                region,
            ));
        }
    }

    /// The ring session this view is showing.
    pub(crate) const fn session_id(&self) -> u64 {
        self.invocation.session_id
    }

    /// Start the visual clock only after native placement has made us visible.
    pub(crate) fn present(&mut self, cx: &mut Context<Self>) {
        self.presented = true;
        self.timing.begin("opening");
        self.motion = Motion::new(Instant::now(), self.invocation.animation_speed);
        cx.notify();
    }
    /// Commands take effect immediately; only presentation waits for the exit.
    pub(crate) fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finish(
            Some(OverlayCommand::Cancel {
                session_id: self.session_id(),
            }),
            window,
            cx,
        );
    }

    /// An agent dismissal has already ended the session; don't acknowledge it.
    pub(crate) fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finish(None, window, cx);
    }

    fn finish(
        &mut self,
        command: Option<OverlayCommand>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.motion.is_closing() {
            return;
        }
        if let Some(command) = command {
            let _ = self.commands.send(command);
        }
        self.region_lease.take();
        self.showing.take();
        self.pointer_position = None;
        self.warped_to = None;
        // Restore the system cursor before the exit animation, not after it:
        // the session is over and the pointer belongs to the user again.
        self.hidden_cursor.take();
        self.timing.begin("closing");
        let exit_duration = self.motion.close(Instant::now(), cx.reduce_motion());
        crate::platform::release_pointer(window);
        if cx.reduce_motion() {
            crate::platform::close_window(window, cx);
        } else {
            let handle = window.window_handle();
            // The session is already over. This task owns visual teardown even
            // if the compositor stops requesting frames while the window closes.
            cx.spawn(async move |_, cx| {
                cx.background_executor().timer(exit_duration).await;
                let _ = handle.update(cx, |_, window, cx| {
                    crate::platform::close_window(window, cx);
                });
            })
            .detach();
            cx.notify();
        }
    }
    fn request_animation_frame(
        &mut self,
        frame: Frame,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if frame.active && self.presented && !self.frame_pending {
            self.frame_pending = true;
            let view = cx.weak_entity();
            window.on_next_frame(move |_, cx| {
                let _ = view.update(cx, |this, cx| {
                    this.frame_pending = false;
                    if this.motion.frame(Instant::now(), cx.reduce_motion()).closed {
                        return;
                    }
                    // A moving wedge can enter/leave a stationary pointer too.
                    if let Some(position) = this.pointer_position.filter(|_| !this.wheel.active()) {
                        let frame = this.motion.frame(Instant::now(), cx.reduce_motion());
                        this.hover(this.hit_slot(position, frame), cx);
                    }
                    cx.notify();
                });
            });
        }
    }

    fn scroll(
        &mut self,
        event: &gpui::ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.motion.is_closing() {
            return;
        }
        cx.stop_propagation();
        let enabled = ActionRingSlot::ALL.map(|slot| self.invocation.slots.contains_key(&slot));
        if let Some(slot) = self.wheel.scroll(
            crate::platform::normalize_ring_scroll(event.delta),
            self.hovered,
            enabled,
        ) {
            // Wheel selection owns the pointer while it is active: put the
            // cursor on the slot it chose, so pointer and selection cannot
            // disagree and a click lands on what the ring is showing.
            let target = Geometry::new(self.bounds.get()).content_center(slot, Pose::REST);
            self.pointer_position = Some(target);
            // The view has moved the pointer as far as it is concerned, whether
            // or not the platform can follow. A move delivered at this position
            // is this selection's own echo, not the user taking the pointer back.
            self.warped_to = Some(target);
            self.warp_cursor_to(target, window);
            self.hover(Some(slot), cx);
        }
    }

    /// Whether this view has a native window whose global rectangle is known.
    /// Both native pointer effects are gated on it: without a published region
    /// there is no screen position to warp to, and no on-screen ring for a
    /// hidden cursor to belong to.
    fn owns_native_pointer(&self) -> bool {
        self.input_region.is_some()
    }

    /// Physically move the system cursor onto a window-local point, in the
    /// global space the input region is published in. Windows publishes
    /// physical pixels, so the local offset is scaled; macOS publishes points
    /// and needs no scaling. Best-effort: where the pointer is *considered* to
    /// be is owned by the caller, not by whether this platform can follow.
    fn warp_cursor_to(&self, target: Point<Pixels>, window: &Window) {
        let Some(region) = &self.input_region else {
            return;
        };
        let scale = if cfg!(target_os = "windows") {
            f64::from(window.scale_factor())
        } else {
            1.0
        };
        crate::platform::warp_cursor(
            region.x + f64::from(f32::from(target.x)) * scale,
            region.y + f64::from(f32::from(target.y)) * scale,
        );
    }

    /// The position the pointer is treated as occupying. Test-only: the warp
    /// target a stationary cursor ends up at after a wheel selection.
    #[cfg(test)]
    pub(crate) const fn pointer_target(&self) -> Option<Point<Pixels>> {
        self.pointer_position
    }

    fn click(
        &mut self,
        position: Point<Pixels>,
        frame: Frame,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        if self.motion.is_closing() {
            return;
        }
        let slot = if self.wheel.active() && Geometry::new(self.bounds.get()).in_ring(position) {
            self.hovered
        } else {
            self.hit_slot(position, frame)
        };
        if let Some(slot) = slot {
            self.finish(
                Some(OverlayCommand::Activate {
                    session_id: self.session_id(),
                    slot,
                }),
                window,
                cx,
            );
        } else {
            self.cancel(window, cx);
        }
    }

    fn hit_slot(&self, position: Point<Pixels>, frame: Frame) -> Option<ActionRingSlot> {
        if self.motion.is_closing() {
            return None;
        }
        let geometry = Geometry::new(self.bounds.get());
        // Last painted is topmost when the entering wedges momentarily overlap.
        ActionRingSlot::ALL.into_iter().rev().find(|slot| {
            self.invocation.slots.contains_key(slot)
                && geometry.contains(position, *slot, frame.sectors[slot.index()].pose)
        })
    }

    /// Report whether `position` is the delivery of this view's own warp, and
    /// consume it. The platforms round the warp target to whole device pixels,
    /// so the echo is compared with a tolerance rather than for equality.
    fn consume_warp_echo(&mut self, position: Point<Pixels>) -> bool {
        let echo = self
            .warped_to
            .is_some_and(|target| is_echo(target, position));
        if echo {
            self.warped_to = None;
        }
        echo
    }

    fn hover(&mut self, slot: Option<ActionRingSlot>, cx: &mut Context<Self>) {
        if self.motion.is_closing() || self.hovered == slot {
            return;
        }
        self.hovered = slot;
        // The ring draws its own pointer over the selected slot, so a second,
        // system-drawn cursor on top of it is redundant. Acquire on the first
        // selection and hold it across slot changes; release once nothing is
        // selected, and unconditionally in `finish`.
        if slot.is_some() && self.owns_native_pointer() {
            if self.hidden_cursor.is_none() {
                self.hidden_cursor = crate::platform::HiddenCursor::acquire();
            }
        } else {
            self.hidden_cursor.take();
        }
        self.motion.select(slot, Instant::now());
        if let Some(slot) = slot {
            let _ = self.commands.send(OverlayCommand::Hover {
                session_id: self.session_id(),
                slot,
            });
        }
        cx.notify();
    }
}

/// A warp lands within rounding distance of its target; a real move that
/// happens to start there is indistinguishable and equally harmless, because
/// the pointer is already on the selected slot either way.
fn is_echo(target: Point<Pixels>, position: Point<Pixels>) -> bool {
    const TOLERANCE: f32 = 2.0;
    f32::from(position.x - target.x).abs() <= TOLERANCE
        && f32::from(position.y - target.y).abs() <= TOLERANCE
}

fn display_label(presentation: &ActionRingPresentation) -> SharedString {
    // A custom label that coincides with an action name is still literal text.
    if !presentation.literal
        && let Some(key) = Action::translation_key_for_label(&presentation.label)
    {
        return rust_i18n::t!(key).into_owned().into();
    }
    presentation.label.clone().into()
}

impl Render for RingView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ring_masks = self
            .ring_masks
            .get_or_insert_with(|| Rc::new(masks::RingMasks::new(window.scale_factor())));
        if !ring_masks.matches_scale(window.scale_factor()) {
            *ring_masks = Rc::new(masks::RingMasks::new(window.scale_factor()));
        }
        let ring_masks = Rc::clone(ring_masks);
        let frame = if self.presented {
            self.motion.frame(Instant::now(), cx.reduce_motion())
        } else {
            self.motion.before_show(cx.reduce_motion())
        };
        self.timing.frame(frame.active);
        self.request_animation_frame(frame, window, cx);
        let stationary_grid = (!frame.active).then(|| PixelGrid::new(window.scale_factor()));
        let enabled = ActionRingSlot::ALL.map(|slot| self.invocation.slots.contains_key(&slot));
        let bounds = Rc::clone(&self.bounds);
        let geometry = Geometry::new(Bounds::new(
            Point::default(),
            gpui::size(px(WINDOW_SIZE), px(WINDOW_SIZE)),
        ));
        let hovered_label = self
            .hovered
            .and_then(|slot| self.invocation.slots.get(&slot))
            .map(display_label);
        let cancel_button =
            paint::cancel(self.hovered.is_some(), &ring_masks).opacity(frame.core_opacity);

        div()
            .id("ring-root")
            .relative()
            .size(px(WINDOW_SIZE))
            .child(
                canvas(
                    move |area, _, _| bounds.set(area),
                    move |area, (), window, cx| {
                        paint::surface(area, enabled, frame, &ring_masks, window, cx);
                    },
                )
                .absolute()
                .size_full(),
            )
            .children(ActionRingSlot::ALL.into_iter().filter_map(|slot| {
                if frame.sectors[slot.index()].pose.opacity <= 0.0 {
                    return None;
                }
                let presentation = self.invocation.slots.get(&slot)?;
                Some(paint::slot_content(
                    geometry,
                    slot,
                    presentation.icon.asset_path(),
                    display_label(presentation),
                    frame.sectors[slot.index()],
                    stationary_grid,
                ))
            }))
            .child(cancel_button.on_click(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                this.cancel(window, cx);
            })))
            .when_some(hovered_label, |ring, label| {
                ring.child(paint::hover_label(label).opacity(frame.core_opacity))
            })
            .on_scroll_wheel(cx.listener(Self::scroll))
            .on_mouse_move(
                cx.listener(move |this, event: &gpui::MouseMoveEvent, _, cx| {
                    if this.pointer_position == Some(event.position) {
                        return;
                    }
                    // A warp this view performed is not the user taking the
                    // pointer back; letting it through would reset the wheel
                    // mode the warp was made for.
                    if this.consume_warp_echo(event.position) {
                        return;
                    }
                    this.wheel.reset();
                    this.pointer_position = Some(event.position);
                    this.hover(this.hit_slot(event.position, frame), cx);
                }),
            )
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !*hovered {
                    this.pointer_position = None;
                    if !this.wheel.active() {
                        this.hover(None, cx);
                    }
                }
            }))
            .on_click(
                cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                    this.click(event.position(), frame, window, cx);
                }),
            )
    }
}

/// Appearance shared by the platform-specific placement paths.
pub(crate) fn ring_window_options() -> WindowOptions {
    WindowOptions {
        titlebar: None,
        focus: false,
        // This nonactivating popup still needs display-rate animation. Idle views
        // stop requesting frames, so disabling the background cap adds no idle loop.
        inactive_frame_interval: None,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        window_background: WindowBackgroundAppearance::Transparent,
        app_id: Some("openlogi-action-ring".to_string()),
        ..WindowOptions::default()
    }
}

#[cfg(any(not(target_os = "windows"), test))]
pub(crate) fn clamp_window_origin(
    desired: Point<Pixels>,
    window_size: Size<Pixels>,
    display: Bounds<Pixels>,
) -> Point<Pixels> {
    let max = point(
        display.right() - window_size.width,
        display.bottom() - window_size.height,
    );
    desired.clamp(&display.origin, &max)
}

#[cfg(test)]
mod tests {
    #[test]
    fn ring_window_does_not_throttle_unfocused_animation() {
        let options = super::ring_window_options();
        assert!(!options.focus);
        assert_eq!(options.inactive_frame_interval, None);
    }

    use super::*;

    #[test]
    fn a_warp_echo_is_absorbed_and_a_real_move_is_not() {
        let target = point(px(100.0), px(120.0));
        // Device-pixel rounding at the platform boundary, not an exact return.
        assert!(is_echo(target, point(px(100.4), px(119.6))));
        assert!(!is_echo(target, point(px(105.0), px(120.0))));
        assert!(!is_echo(target, point(px(100.0), px(130.0))));
    }

    #[test]
    fn custom_labels_are_not_translated_or_normalized() {
        let presentation = ActionRingPresentation {
            label: "Copy".into(),
            literal: true,
            icon: openlogi_core::binding::ActionRingIcon::Copy,
        };
        assert_eq!(display_label(&presentation).as_ref(), "Copy");
        let presentation = ActionRingPresentation {
            label: "  自定义 action <&>  ".into(),
            ..presentation
        };
        assert_eq!(
            display_label(&presentation).as_ref(),
            "  自定义 action <&>  "
        );
    }

    #[test]
    fn built_in_labels_use_the_shared_catalog() {
        let presentation = ActionRingPresentation {
            label: "Copy".into(),
            literal: false,
            icon: openlogi_core::binding::ActionRingIcon::Copy,
        };
        let key = Action::translation_key_for_label("Copy").expect("known action");
        assert_eq!(
            display_label(&presentation).as_ref(),
            rust_i18n::t!(key).as_ref()
        );
    }

    #[test]
    fn overlay_origin_is_clamped_to_the_display() {
        let display = Bounds::new(point(px(100.0), px(50.0)), Size::new(px(800.0), px(600.0)));
        let size = Size::new(px(400.0), px(400.0));
        assert_eq!(
            clamp_window_origin(point(px(-50.0), px(-50.0)), size, display),
            point(px(100.0), px(50.0))
        );
        assert_eq!(
            clamp_window_origin(point(px(700.0), px(500.0)), size, display),
            point(px(500.0), px(250.0))
        );
    }

    #[test]
    fn overlay_origin_stays_cursor_centered_away_from_edges() {
        let display = Bounds::new(Point::default(), Size::new(px(1600.0), px(1000.0)));
        let desired = point(px(600.0), px(300.0));
        assert_eq!(
            clamp_window_origin(desired, Size::new(px(400.0), px(400.0)), display),
            desired
        );
    }
}
