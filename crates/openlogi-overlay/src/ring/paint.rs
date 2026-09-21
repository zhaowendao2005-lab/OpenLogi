//! Overlay-local tokens and native vector/text composition.
//!
//! This surface deliberately stays dark over arbitrary desktop backgrounds.
//! Reference geometry uses fixed logical pixels to match its external 520-unit
//! drawing; readable text and the cancel target retain native utility sizing.

use gpui::{
    App, Bounds, Hsla, IntoElement, ParentElement, PathBuilder, Pixels, SharedString, Styled,
    Window, div, point, px, rgb, svg,
};
use openlogi_core::binding::ActionRingSlot;

use super::geometry::{Geometry, PixelGrid, Pose, slot_angle};
use super::masks::WedgeMasks;
use super::motion::{Frame, SectorFrame};

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
    masks: &WedgeMasks,
    window: &mut Window,
    cx: &App,
) {
    let geometry = Geometry::new(area);
    let palette = Palette::ring();
    let grid = PixelGrid::new(window.scale_factor());
    guides(geometry, grid, frame.guides_opacity, palette, window);
    for slot in ActionRingSlot::ALL {
        if !enabled[slot.index()] {
            continue;
        }
        let sector = frame.sectors[slot.index()];
        if sector.pose.opacity <= 0.0 {
            continue;
        }
        masks.paint(
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
        let pose = Pose {
            rotation: frame.pointer_angle,
            ..Pose::REST
        };
        let tip = geometry.point(223.0, 0.0, pose);
        let mut pointer = PathBuilder::fill();
        pointer.move_to(geometry.point(231.0, -1.5, pose));
        pointer.line_to(geometry.point(231.0, 1.5, pose));
        pointer.line_to(tip);
        pointer.close();
        path(
            pointer,
            palette.selected.opacity(frame.pointer_opacity),
            window,
        );
        let width = grid.stroke_width(geometry.length(1.0));
        let mut stem = PathBuilder::stroke(width);
        let (start, end) = grid.line(
            geometry.point(217.0, 0.0, pose),
            geometry.point(209.0, 0.0, pose),
            width,
        );
        stem.move_to(start);
        stem.line_to(end);
        path(stem, palette.guide.opacity(frame.pointer_opacity), window);
    }
}

fn guides(
    geometry: Geometry,
    grid: PixelGrid,
    opacity: f32,
    palette: Palette,
    window: &mut Window,
) {
    for (radius, dashed) in [(235.0, false), (216.0, true), (78.0, false)] {
        let mut circle = PathBuilder::stroke(grid.stroke_width(geometry.length(0.65)));
        if dashed {
            circle = circle.dash_array(&[geometry.length(2.0), geometry.length(5.0)]);
        }
        geometry.circle(radius, &mut circle);
        path(circle, palette.guide.opacity(0.6 * opacity), window);
    }
    let width = grid.stroke_width(geometry.length(1.0));
    let mut ticks = PathBuilder::stroke(width);
    for i in 0_u16..80 {
        let major = i % 10 == 0;
        let angle = f32::from(i) * 4.5;
        let (start, end) = grid.line(
            geometry.point(if major { 239.0 } else { 240.0 }, angle, Pose::REST),
            geometry.point(if major { 248.0 } else { 244.0 }, angle, Pose::REST),
            width,
        );
        ticks.move_to(start);
        ticks.line_to(end);
    }
    path(ticks, palette.guide.opacity(0.85 * opacity), window);
    let mut spokes = PathBuilder::stroke(grid.stroke_width(geometry.length(0.7)));
    for slot in ActionRingSlot::ALL {
        let angle = slot_angle(slot) + 22.5;
        spokes.move_to(geometry.point(80.0, angle, Pose::REST));
        spokes.line_to(geometry.point(230.0, angle, Pose::REST));
    }
    path(spokes, palette.guide.opacity(0.45 * opacity), window);
}

fn path(builder: PathBuilder, color: Hsla, window: &mut Window) {
    match builder.build() {
        Ok(path) => window.paint_path(path, color),
        Err(error) => tracing::warn!(?error, "could not tessellate Actions Ring path"),
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
    let origin = align(center - point(px(30.0 * scale), px(28.0 * scale)));
    let icon_origin = align(origin + point(px(19.0 * scale), px(13.0 * scale))) - origin;
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
        .w(px(60.0 * scale))
        .h(px(56.0 * scale))
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
                .size(px(22.0 * scale))
                .path(icon)
                .text_color(foreground),
        )
        .child(
            div()
                .absolute()
                .top(title_origin.y)
                .w_full()
                .text_center()
                .text_size(px(12.0 * scale))
                .line_height(px(16.0 * scale))
                .truncate()
                .child(label),
        )
}

pub(super) fn cancel(selected: bool) -> gpui::Stateful<gpui::Div> {
    use gpui::{InteractiveElement as _, StatefulInteractiveElement as _};
    let palette = Palette::ring();
    div()
        .id("ring-cancel")
        .aria_label(rust_i18n::t!("common.cancel").into_owned())
        .absolute()
        .left(px(super::WINDOW_SIZE / 2.0 - 24.0))
        .top(px(super::WINDOW_SIZE / 2.0 - 24.0))
        .size(px(48.0))
        .rounded_full()
        .border_1()
        .border_color(palette.border)
        .bg(palette.plate)
        .flex()
        .items_center()
        .justify_center()
        .hover(|style| style.border_color(palette.selected))
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

pub(super) fn hover_label(label: SharedString) -> impl IntoElement {
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
