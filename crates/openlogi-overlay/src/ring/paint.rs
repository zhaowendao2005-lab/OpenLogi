//! Overlay-local tokens and native vector/text composition.
//!
//! This surface deliberately stays dark over arbitrary desktop backgrounds.
//! Reference geometry uses fixed logical pixels to match its external 520-unit
//! drawing; readable text and the cancel target retain native utility sizing.

use gpui::{
    App, Bounds, Hsla, IntoElement, ParentElement, Pixels, SharedString, Styled, Window, div,
    point, px, rgb, svg,
};
use openlogi_core::binding::ActionRingSlot;

use super::geometry::{CANCEL_SIZE, Geometry, PixelGrid, Pose};
use super::masks::RingMasks;
use super::motion::{Frame, SectorFrame};

/// One slot's content box and its contents, in the same fixed logical pixels as
/// the reference geometry. The icon's horizontal offset is derived from these
/// rather than written out, so a resized icon stays centred in the box.
const CONTENT_WIDTH: f32 = 60.0;
const CONTENT_HEIGHT: f32 = 56.0;
const ICON_SIZE: f32 = 20.0;
const ICON_TOP: f32 = 13.0;
const LABEL_SIZE: f32 = 11.0;
const LABEL_LINE_HEIGHT: f32 = 15.0;

#[derive(Clone, Copy)]
pub(super) struct Palette {
    pub plate: Hsla,
    pub border: Hsla,
    pub foreground: Hsla,
    pub muted: Hsla,
    pub selected: Hsla,
    pub selected_foreground: Hsla,
    pub guide: Hsla,
}

impl Palette {
    pub fn ring() -> Self {
        Self {
            plate: rgb(0x20_2124).into(),
            border: rgb(0x73_747a).into(),
            foreground: rgb(0xed_edee).into(),
            muted: rgb(0xbc_bcc2).into(),
            selected: rgb(0xfb_fb45).into(),
            selected_foreground: rgb(0x18_181b).into(),
            guide: rgb(0x88_888f).into(),
        }
    }
}

pub(super) fn surface(
    area: Bounds<Pixels>,
    enabled: [bool; 8],
    frame: Frame,
    masks: &RingMasks,
    window: &mut Window,
    cx: &App,
) {
    let palette = Palette::ring();
    masks.paint_guides(area, frame.guides, palette.guide, window, cx);
    for slot in ActionRingSlot::ALL {
        if !enabled[slot.index()] {
            continue;
        }
        let sector = frame.sectors[slot.index()];
        if sector.pose.opacity <= 0.0 {
            continue;
        }
        masks.paint_wedge(
            slot,
            area,
            sector.pose,
            [
                palette
                    .plate
                    .blend(palette.selected.opacity(sector.highlight)),
                palette
                    .border
                    .blend(palette.selected.opacity(sector.highlight)),
            ],
            window,
            cx,
        );
    }
    if frame.pointer_opacity > 0.0 {
        masks.paint_pointer(
            area,
            Pose {
                rotation: frame.pointer_angle,
                opacity: frame.pointer_opacity,
                ..Pose::REST
            },
            [palette.selected, palette.guide],
            window,
            cx,
        );
    }
}
pub(super) fn slot_content(
    geometry: Geometry,
    slot: ActionRingSlot,
    icon: &'static str,
    label: SharedString,
    frame: SectorFrame,
    stationary_grid: Option<PixelGrid>,
) -> impl IntoElement {
    let palette = Palette::ring();
    let center = geometry.content_center(slot, frame.pose);
    let scale = frame.content_scale * frame.pose.scale;
    let align = |position| stationary_grid.map_or(position, |grid| grid.snap(position));
    let origin = align(center - point(px(CONTENT_WIDTH / 2.0 * scale), px(28.0 * scale)));
    let icon_origin = align(
        origin
            + point(
                px((CONTENT_WIDTH - ICON_SIZE) / 2.0 * scale),
                px(ICON_TOP * scale),
            ),
    ) - origin;
    let title_origin = align(origin + point(px(0.0), px(40.0 * scale))) - origin;
    let foreground = palette
        .foreground
        .blend(palette.selected_foreground.opacity(frame.highlight));
    let muted = palette
        .muted
        .blend(palette.selected_foreground.opacity(frame.highlight));
    div()
        .absolute()
        .left(origin.x)
        .top(origin.y)
        .w(px(CONTENT_WIDTH * scale))
        .h(px(CONTENT_HEIGHT * scale))
        .opacity(frame.pose.opacity)
        .text_color(foreground)
        .child(
            div()
                .absolute()
                .top_0()
                .w_full()
                .text_center()
                .text_size(px(9.0 * scale))
                .line_height(px(11.0 * scale))
                .text_color(muted)
                .child(format!("{:02}", slot.index() + 1)),
        )
        .child(
            svg()
                .absolute()
                .left(icon_origin.x)
                .top(icon_origin.y)
                .size(px(ICON_SIZE * scale))
                .path(icon)
                .text_color(foreground),
        )
        .child(
            div()
                .absolute()
                .top(title_origin.y)
                .w_full()
                .text_center()
                .text_size(px(LABEL_SIZE * scale))
                .line_height(px(LABEL_LINE_HEIGHT * scale))
                .truncate()
                .child(label),
        )
}

pub(super) fn cancel(selected: bool, masks: &RingMasks) -> gpui::Stateful<gpui::Div> {
    use gpui::{InteractiveElement as _, StatefulInteractiveElement as _};
    let palette = Palette::ring();
    let [fill, outline] = masks.cancel_layers();
    div()
        .id("ring-cancel")
        .group("ring-cancel")
        .aria_label(rust_i18n::t!("common.cancel").into_owned())
        .absolute()
        .left(px((super::WINDOW_SIZE - CANCEL_SIZE) / 2.0))
        .top(px((super::WINDOW_SIZE - CANCEL_SIZE) / 2.0))
        .size(px(CANCEL_SIZE))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .child(
            fill.absolute()
                .top_0()
                .left_0()
                .size_full()
                .text_color(palette.plate),
        )
        .child(
            outline
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .text_color(palette.border)
                .group_hover("ring-cancel", |style| style.text_color(palette.selected)),
        )
        .child(
            svg()
                .path(openlogi_ui::action_icons::RING_CANCEL_ICON)
                .size(px(20.0))
                .text_color(if selected {
                    palette.selected
                } else {
                    palette.muted
                }),
        )
}

pub(super) fn hover_label(label: SharedString) -> gpui::Div {
    let palette = Palette::ring();
    div()
        .absolute()
        .left(px(super::WINDOW_SIZE / 2.0 - 42.0))
        .top(px(super::WINDOW_SIZE / 2.0 + 26.0))
        .w(px(84.0))
        .line_clamp(2)
        .rounded_sm()
        .bg(palette.plate)
        .text_center()
        .text_size(px(12.0))
        .line_height(px(14.0))
        .text_color(palette.foreground)
        .child(label)
}
