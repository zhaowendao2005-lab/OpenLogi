/// The catalog this binary translates against lives in `openlogi-ui` and is
/// reached by the relative path in the `i18n!` in `main.rs`. A wrong path does
/// **not** fail the build: `rust_i18n` compiles it to an empty catalog, and
/// every ring label renders as its semantic key. Resolve one action key so that
/// breakage is loud without pinning any translation wording.
#[test]
fn the_shared_catalog_is_wired_up() {
    const KEY: &str = "actions.left_click";
    assert_ne!(rust_i18n::t!(KEY), KEY);
}

use std::{collections::BTreeMap, sync::Arc};

use gpui::{
    AppContext as _, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    PlatformInput, Point, TestAppContext, WindowHandle, point, px, size,
};
use openlogi_core::binding::{ActionRingIcon, ActionRingSlot};
use openlogi_ipc::{ActionRingInvocation, ActionRingPresentation};
use tokio::sync::mpsc;

use crate::{ipc::OverlayCommand, ring::RingView, session::ClickAwaySession};

fn invocation(slots: impl IntoIterator<Item = ActionRingSlot>) -> ActionRingInvocation {
    ActionRingInvocation {
        animation_speed: openlogi_core::binding::ActionRingAnimationSpeed::default(),
        session_id: 7,
        slots: slots
            .into_iter()
            .map(|slot| {
                (
                    slot,
                    ActionRingPresentation {
                        label: "Copy".into(),
                        literal: true,
                        icon: ActionRingIcon::Copy,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>(),
        language: None,
    }
}

fn open_ring(
    cx: &mut TestAppContext,
    invocation: ActionRingInvocation,
) -> (
    WindowHandle<RingView>,
    mpsc::UnboundedReceiver<OverlayCommand>,
) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let (commands, received) = mpsc::unbounded_channel();
    let live = Arc::new(ClickAwaySession::new());
    let window = cx.open_window(size(px(360.0), px(360.0)), move |_, _| {
        RingView::new(invocation, commands, &live)
    });
    cx.run_until_parked();
    (window, received)
}

fn move_to(cx: &mut TestAppContext, window: WindowHandle<RingView>, position: Point<Pixels>) {
    cx.update_window(window.into(), |_, window, cx| {
        window.dispatch_event(
            PlatformInput::MouseMove(MouseMoveEvent {
                position,
                ..Default::default()
            }),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
}

fn click(cx: &mut TestAppContext, window: WindowHandle<RingView>, position: Point<Pixels>) {
    cx.update_window(window.into(), |_, window, cx| {
        window.dispatch_event(
            PlatformInput::MouseDown(MouseDownEvent {
                position,
                button: MouseButton::Left,
                ..Default::default()
            }),
            cx,
        );
        window.dispatch_event(
            PlatformInput::MouseUp(MouseUpEvent {
                position,
                button: MouseButton::Left,
                ..Default::default()
            }),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
}

#[gpui::test]
fn vector_ring_hover_is_deduplicated_and_click_only_activates(cx: &mut TestAppContext) {
    let (window, mut received) = open_ring(cx, invocation(ActionRingSlot::ALL));
    let top = point(px(180.0), px(80.0));
    move_to(cx, window, top);
    move_to(cx, window, point(px(181.0), px(80.0)));
    assert!(matches!(
        received.try_recv().unwrap(),
        OverlayCommand::Hover {
            session_id: 7,
            slot: ActionRingSlot::Top
        }
    ));
    assert!(matches!(
        received.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    click(cx, window, top);
    assert!(matches!(
        received.try_recv().unwrap(),
        OverlayCommand::Activate {
            session_id: 7,
            slot: ActionRingSlot::Top
        }
    ));
    assert!(
        received.try_recv().is_err(),
        "activation must not bubble into cancellation"
    );
    assert!(cx.update(|cx| cx.windows().is_empty()));
}

#[gpui::test]
fn vector_ring_gap_and_unassigned_slot_cancel_without_activation(cx: &mut TestAppContext) {
    for position in [point(px(280.0), px(180.0)), point(px(218.3), px(87.6))] {
        let (window, mut received) = open_ring(cx, invocation([ActionRingSlot::Top]));
        move_to(cx, window, position);
        assert!(matches!(
            received.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        click(cx, window, position);
        assert!(matches!(
            received.try_recv().unwrap(),
            OverlayCommand::Cancel { session_id: 7 }
        ));
        assert!(matches!(
            received.try_recv(),
            Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected)
        ));
    }
}

#[gpui::test]
fn vector_ring_cancel_center_sends_one_terminal_command(cx: &mut TestAppContext) {
    // Both the icon and the new SVG rim must bubble to the existing cancel target.
    for position in [
        point(px(180.0), px(180.0)),
        point(px(180.0), px(157.0)),
        point(px(203.0), px(180.0)),
    ] {
        let (window, mut received) = open_ring(cx, invocation(ActionRingSlot::ALL));
        move_to(cx, window, point(px(180.0), px(80.0)));
        assert!(matches!(
            received.try_recv().unwrap(),
            OverlayCommand::Hover { .. }
        ));
        move_to(cx, window, position);
        click(cx, window, position);
        assert!(matches!(
            received.try_recv().unwrap(),
            OverlayCommand::Cancel { session_id: 7 }
        ));
        assert!(matches!(
            received.try_recv(),
            Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected)
        ));
        assert!(cx.update(|cx| cx.windows().is_empty()));
    }
}
#[gpui::test]
fn vector_ring_returning_from_hole_publishes_hover_again(cx: &mut TestAppContext) {
    let (window, mut received) = open_ring(cx, invocation(ActionRingSlot::ALL));
    move_to(cx, window, point(px(180.0), px(80.0)));
    move_to(cx, window, point(px(180.0), px(145.0)));
    move_to(cx, window, point(px(180.0), px(80.0)));
    for _ in 0..2 {
        assert!(matches!(
            received.try_recv().unwrap(),
            OverlayCommand::Hover {
                slot: ActionRingSlot::Top,
                ..
            }
        ));
    }
    assert!(matches!(
        received.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
}

#[gpui::test]
fn vector_ring_reduced_motion_requests_no_animation_frames(cx: &mut TestAppContext) {
    let (window, _) = open_ring(cx, invocation(ActionRingSlot::ALL));
    let frames = window
        .update(cx, |_, window, cx| window.simulate_next_frame(cx))
        .unwrap();
    assert_eq!(frames, 0);
}

#[gpui::test]
fn closing_sends_once_and_its_timer_cannot_close_a_replacement(cx: &mut TestAppContext) {
    let (old, mut commands) = open_ring(cx, invocation(ActionRingSlot::ALL));
    cx.update(|cx| cx.set_reduce_motion(false));
    old.update(cx, |view, _, cx| view.present(cx)).unwrap();
    click(cx, old, point(px(180.0), px(180.0)));
    assert!(matches!(
        commands.try_recv().unwrap(),
        OverlayCommand::Cancel { session_id: 7 }
    ));
    old.update(cx, |view, window, cx| {
        view.cancel(window, cx);
        view.dismiss(window, cx);
    })
    .unwrap();
    assert!(matches!(
        commands.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    assert_eq!(
        cx.update(|cx| cx.windows().len()),
        1,
        "visual exit must not delay the command"
    );
    let mut next = invocation(ActionRingSlot::ALL);
    next.session_id = 8;
    let (replacement, _) = open_ring(cx, next);
    cx.background_executor
        .advance_clock(std::time::Duration::from_secs(1));
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| cx.windows().len()), 1);
    assert_eq!(
        replacement
            .read_with(cx, |view, _| view.session_id())
            .unwrap(),
        8
    );
}
/// Exercises the actual GPU/window/asset path, without a mock agent or real actions.
#[test]
#[ignore = "opens a native window; run manually for screenshot and pointer verification"]
fn vector_ring_native_preview() {
    openlogi_core::logging::init_stderr();
    let app = gpui_platform::application().with_assets(openlogi_ui::action_icons::ActionIcons);
    app.run(|cx| {
        let mut invocation = invocation(ActionRingSlot::ALL);
        for (slot, label) in ActionRingSlot::ALL.into_iter().zip([
            "剪切",
            "复制",
            "粘贴",
            "前进",
            "播放 / 暂停",
            "后退",
            "撤销",
            "重做",
        ]) {
            let entry = invocation.slots.get_mut(&slot).unwrap();
            entry.label = label.into();
            entry.icon = match slot {
                ActionRingSlot::Top => ActionRingIcon::Cut,
                ActionRingSlot::TopRight => ActionRingIcon::Copy,
                ActionRingSlot::Right => ActionRingIcon::Paste,
                ActionRingSlot::BottomRight => ActionRingIcon::ArrowRight,
                ActionRingSlot::Bottom => ActionRingIcon::Play,
                ActionRingSlot::BottomLeft => ActionRingIcon::ArrowLeft,
                ActionRingSlot::Left => ActionRingIcon::Undo,
                ActionRingSlot::TopLeft => ActionRingIcon::Redo,
            };
        }
        let (commands, _received) = mpsc::unbounded_channel();
        let live = Arc::new(ClickAwaySession::new());
        crate::platform::configure_application();
        let placement = crate::platform::RingPlacement::capture(cx).unwrap();
        let handle = cx
            .open_window(placement.window_options(), |_, cx| {
                cx.new(|_| RingView::new(invocation, commands, &live))
            })
            .unwrap();
        cx.spawn(async move |cx| {
            placement.show(handle, cx).unwrap();
        })
        .detach();
    });
}

/// Reproduces show/dismiss/replacement on the real native event loop, including
/// delayed callbacks; no IPC server, device, or action execution is involved.
#[test]
#[ignore = "manual native lifecycle regression; opens and closes ten windows"]
fn vector_ring_native_lifecycle() {
    openlogi_core::logging::init_stderr();
    gpui_platform::application()
        .with_assets(openlogi_ui::action_icons::ActionIcons)
        .with_quit_mode(gpui::QuitMode::Explicit)
        .run(|cx| {
            crate::platform::configure_application();
            let live = Arc::new(ClickAwaySession::new());
            cx.spawn(async move |cx| {
                for id in 1..=10 {
                    let started = std::time::Instant::now();
                    let mut request = invocation(ActionRingSlot::ALL);
                    request.session_id = id;
                    let (commands, _received) = mpsc::unbounded_channel();
                    let (placement, handle) = cx.update(|cx| {
                        for old in cx.windows() {
                            let _ = old.update(cx, |_, window, cx| {
                                crate::platform::close_window(window, cx);
                            });
                        }
                        let placement = crate::platform::RingPlacement::capture(cx).unwrap();
                        let handle = cx
                            .open_window(placement.window_options(), |_, cx| {
                                cx.new(|_| RingView::new(request, commands, &live))
                            })
                            .unwrap();
                        (placement, handle)
                    });
                    placement.show(handle, cx).unwrap();
                    eprintln!("cycle {id}: window ready {:?}", started.elapsed());
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(450))
                        .await;
                    handle.update(cx, RingView::dismiss).unwrap();
                    let pause = if id % 3 == 0 { 25 } else { 250 };
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(pause))
                        .await;
                    if pause == 250 {
                        assert!(cx.update(|cx| cx.windows().is_empty()));
                    }
                }
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
}

#[gpui::test]
fn vector_ring_wheel_selection_survives_stationary_mouse_and_confirms(cx: &mut TestAppContext) {
    let (window, mut received) = open_ring(cx, invocation(ActionRingSlot::ALL));
    let top = point(px(180.), px(80.));
    move_to(cx, window, top);
    assert!(matches!(
        received.try_recv().unwrap(),
        OverlayCommand::Hover {
            slot: ActionRingSlot::Top,
            ..
        }
    ));
    cx.update_window(window.into(), |_, window, cx| {
        window.dispatch_event(
            PlatformInput::ScrollWheel(gpui::ScrollWheelEvent {
                position: top,
                // Pixels bypass platform line/notch normalization and exercise accumulation.
                delta: gpui::ScrollDelta::Pixels(point(px(0.), px(-40.))),
                ..Default::default()
            }),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    assert!(matches!(
        received.try_recv().unwrap(),
        OverlayCommand::Hover {
            slot: ActionRingSlot::TopRight,
            ..
        }
    ));
    // Wheel selection warps the pointer onto the slot it chose, so a cursor the
    // user has not touched now reports the slot centre. The move the platform
    // delivers for that warp, rounded to device pixels, must not be mistaken for
    // the user taking the pointer back and must not disturb the selection.
    let warped = window
        .read_with(cx, |view, _| view.pointer_target())
        .unwrap()
        .expect("wheel selection places the pointer on the selected slot");
    move_to(cx, window, warped + point(px(0.4), px(-0.4)));
    received.try_recv().unwrap_err();
    click(cx, window, top);
    assert!(matches!(
        received.try_recv().unwrap(),
        OverlayCommand::Activate {
            slot: ActionRingSlot::TopRight,
            ..
        }
    ));
}

#[gpui::test]
fn vector_ring_wheel_then_outside_click_cancels(cx: &mut TestAppContext) {
    let (window, mut received) = open_ring(cx, invocation(ActionRingSlot::ALL));
    let top = point(px(180.), px(80.));
    cx.update_window(window.into(), |_, window, cx| {
        window.dispatch_event(
            PlatformInput::ScrollWheel(gpui::ScrollWheelEvent {
                position: top,
                delta: gpui::ScrollDelta::Pixels(point(px(0.), px(-40.))),
                ..Default::default()
            }),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    assert!(matches!(
        received.try_recv().unwrap(),
        OverlayCommand::Hover { .. }
    ));
    click(cx, window, point(px(5.), px(5.)));
    assert!(matches!(
        received.try_recv().unwrap(),
        OverlayCommand::Cancel { .. }
    ));
}
