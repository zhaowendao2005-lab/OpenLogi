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
    let (window, mut received) = open_ring(cx, invocation(ActionRingSlot::ALL));
    click(cx, window, point(px(180.0), px(180.0)));
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

/// Exercises the actual GPU/window/asset path, without a mock agent or real actions.
#[test]
#[ignore = "opens a native window; run manually for screenshot and pointer verification"]
fn vector_ring_native_preview() {
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
        handle
            .update(cx, |_, window, _| placement.show(window))
            .unwrap()
            .unwrap();
    });
}
