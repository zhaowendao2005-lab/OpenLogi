//! Native vector presentation for the cursor-centred Actions Ring.
//! Session commands and window placement remain owned by their existing layers.

mod geometry;
mod masks;
mod motion;
mod paint;

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
use geometry::{Geometry, PixelGrid};
use motion::{Frame, Motion};

pub(crate) const WINDOW_SIZE: f32 = 360.0;

pub(crate) struct RingView {
    invocation: ActionRingInvocation,
    commands: mpsc::UnboundedSender<OverlayCommand>,
    hovered: Option<ActionRingSlot>,
    motion: Motion,
    wedge_masks: Option<Rc<masks::WedgeMasks>>,
    pointer_position: Option<Point<Pixels>>,
    /// Layout bookkeeping written by the canvas, not another selection owner.
    bounds: Rc<Cell<Bounds<Pixels>>>,
    /// Publishes click-away identity for exactly this view's lifetime.
    _showing: ShowingRing,
}

impl RingView {
    /// Open a view on `invocation`, reporting interactions through `commands`.
    pub(crate) fn new(
        invocation: ActionRingInvocation,
        commands: mpsc::UnboundedSender<OverlayCommand>,
        live: &Arc<ClickAwaySession>,
    ) -> Self {
        let showing = live.showing(invocation.session_id);
        Self {
            invocation,
            commands,
            hovered: None,
            motion: Motion::new(Instant::now()),
            wedge_masks: None,
            pointer_position: None,
            bounds: Rc::new(Cell::new(Bounds::default())),
            _showing: showing,
        }
    }

    /// The ring session this view is showing.
    pub(crate) const fn session_id(&self) -> u64 {
        self.invocation.session_id
    }

    /// Report this ring cancelled; the handle owner closes the window.
    pub(crate) fn cancel(&self) {
        let _ = self.commands.send(OverlayCommand::Cancel {
            session_id: self.session_id(),
        });
    }

    fn hit_slot(&self, position: Point<Pixels>, frame: Frame) -> Option<ActionRingSlot> {
        let geometry = Geometry::new(self.bounds.get());
        // Last painted is topmost when the entering wedges momentarily overlap.
        ActionRingSlot::ALL.into_iter().rev().find(|slot| {
            self.invocation.slots.contains_key(slot)
                && geometry.contains(position, *slot, frame.sectors[slot.index()].pose)
        })
    }

    fn hover(&mut self, slot: Option<ActionRingSlot>, cx: &mut Context<Self>) {
        if self.hovered == slot {
            return;
        }
        self.hovered = slot;
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
        let wedge_masks = self
            .wedge_masks
            .get_or_insert_with(|| Rc::new(masks::WedgeMasks::new(window.scale_factor())));
        if !wedge_masks.matches_scale(window.scale_factor()) {
            *wedge_masks = Rc::new(masks::WedgeMasks::new(window.scale_factor()));
        }
        let wedge_masks = Rc::clone(wedge_masks);
        let frame = self.motion.frame(Instant::now(), cx.reduce_motion());
        if frame.active {
            let view = cx.weak_entity();
            window.on_next_frame(move |_, cx| {
                let _ = view.update(cx, |this, cx| {
                    // A moving wedge can enter/leave a stationary pointer too.
                    if let Some(position) = this.pointer_position {
                        let frame = this.motion.frame(Instant::now(), cx.reduce_motion());
                        this.hover(this.hit_slot(position, frame), cx);
                    }
                    cx.notify();
                });
            });
        }
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
        let cancel_commands = self.commands.clone();
        let session_id = self.session_id();

        div()
            .id("ring-root")
            .relative()
            .size(px(WINDOW_SIZE))
            .child(
                canvas(
                    move |area, _, _| bounds.set(area),
                    move |area, (), window, cx| {
                        paint::surface(area, enabled, frame, &wedge_masks, window, cx);
                    },
                )
                .absolute()
                .size_full(),
            )
            .children(ActionRingSlot::ALL.into_iter().filter_map(|slot| {
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
            .child(
                paint::cancel(self.hovered.is_some()).on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    let _ = cancel_commands.send(OverlayCommand::Cancel { session_id });
                    window.remove_window();
                }),
            )
            .when_some(hovered_label, |ring, label| {
                ring.child(paint::hover_label(label))
            })
            .on_mouse_move(
                cx.listener(move |this, event: &gpui::MouseMoveEvent, _, cx| {
                    this.pointer_position = Some(event.position);
                    this.hover(this.hit_slot(event.position, frame), cx);
                }),
            )
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !*hovered {
                    this.pointer_position = None;
                    this.hover(None, cx);
                }
            }))
            .on_click(
                cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                    cx.stop_propagation();
                    if let Some(slot) = this.hit_slot(event.position(), frame) {
                        let _ = this.commands.send(OverlayCommand::Activate {
                            session_id: this.session_id(),
                            slot,
                        });
                    } else {
                        this.cancel();
                    }
                    window.remove_window();
                }),
            )
    }
}

/// Appearance shared by the platform-specific placement paths.
pub(crate) fn ring_window_options() -> WindowOptions {
    WindowOptions {
        titlebar: None,
        focus: false,
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
    use super::*;

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
