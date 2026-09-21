//! Fixed SVG masks for the ring's curved edges. GPUI owns rasterization and its
//! DPI-sized atlas; only color and a bounded pose transform change per frame.

use gpui::{
    App, Bounds, Hsla, Pixels, Point, SharedString, TransformationMatrix, Window, px, radians, size,
};
use openlogi_core::binding::ActionRingSlot;

use super::WINDOW_SIZE;
use super::geometry::{Geometry, PixelGrid, Pose, cancel_svgs, square};

struct Mask {
    key: SharedString,
    data: String,
    viewport: Bounds<Pixels>,
}

impl Mask {
    fn paint(
        &self,
        bounds: Bounds<Pixels>,
        transform: TransformationMatrix,
        color: Hsla,
        window: &mut Window,
        cx: &App,
    ) {
        if let Err(error) = window.paint_svg(
            Bounds::new(bounds.origin + self.viewport.origin, self.viewport.size),
            self.key.clone(),
            Some(self.data.as_bytes()),
            transform,
            color,
            cx,
        ) {
            tracing::warn!(?error, "could not paint Actions Ring SVG mask");
        }
    }
}
pub(super) struct RingMasks {
    scale_factor: f32,
    slots: [[Mask; 2]; 8],
    guides: Mask,
    pointer: [Mask; 2],
    cancel: [String; 2],
}

impl RingMasks {
    pub fn new(scale_factor: f32) -> Self {
        let geometry = Geometry::new(Bounds::new(
            Point::default(),
            size(px(WINDOW_SIZE), px(WINDOW_SIZE)),
        ));
        let grid = PixelGrid::new(scale_factor);
        let width = grid.stroke_width(geometry.length(0.8));
        Self {
            scale_factor,
            guides: Mask {
                key: format!("__openlogi_ring_guides_{}", scale_factor.to_bits()).into(),
                data: geometry.guides_svg(grid),
                viewport: square(WINDOW_SIZE),
            },
            pointer: {
                let viewport = geometry.pointer_viewport(grid);
                let [tip, stem] = geometry.pointer_svgs(grid, viewport);
                [("tip", tip), ("stem", stem)].map(|(part, data)| Mask {
                    key: format!("__openlogi_ring_pointer_{part}_{}", scale_factor.to_bits())
                        .into(),
                    data,
                    viewport,
                })
            },
            cancel: cancel_svgs(),
            slots: ActionRingSlot::ALL.map(|slot| {
                let viewport = geometry.wedge_viewport(slot, grid);
                [None, Some(width)].map(|stroke| Mask {
                    // Namespaced identities stay constant through animation. The
                    // DPI affects the cropped viewport as well as the stroke width.
                    key: format!(
                        "__openlogi_ring_wedge_{}_{}_{}",
                        slot.index(),
                        scale_factor.to_bits(),
                        stroke.map_or(0, |width| f32::from(width).to_bits()),
                    )
                    .into(),
                    data: geometry.wedge_svg(slot, stroke, viewport),
                    viewport,
                })
            }),
        }
    }

    pub fn paint_guides(
        &self,
        bounds: Bounds<Pixels>,
        pose: Pose,
        color: Hsla,
        window: &mut Window,
        cx: &App,
    ) {
        if pose.opacity <= 0.0 {
            return;
        }
        self.guides.paint(
            bounds,
            pose_transform(bounds, pose, window.scale_factor()),
            color.opacity(pose.opacity),
            window,
            cx,
        );
    }
    pub fn paint_pointer(
        &self,
        bounds: Bounds<Pixels>,
        pose: Pose,
        colors: [Hsla; 2],
        window: &mut Window,
        cx: &App,
    ) {
        let transform = pose_transform(bounds, pose, window.scale_factor());
        for (mask, color) in self.pointer.iter().zip(colors) {
            mask.paint(bounds, transform, color.opacity(pose.opacity), window, cx);
        }
    }

    /// Return owned elements; the caller attaches the button's hover styling.
    pub fn cancel_layers(&self) -> [gpui::Svg; 2] {
        self.cancel
            .each_ref()
            .map(|data| gpui::svg().data(data.as_bytes()))
    }
    pub fn matches_scale(&self, scale_factor: f32) -> bool {
        self.scale_factor.to_bits() == scale_factor.to_bits()
    }

    pub fn paint_wedge(
        &self,
        slot: ActionRingSlot,
        bounds: Bounds<Pixels>,
        pose: Pose,
        colors: [Hsla; 2],
        window: &mut Window,
        cx: &App,
    ) {
        let transform = pose_transform(bounds, pose, window.scale_factor());
        for (mask, color) in self.slots[slot.index()].iter().zip(colors) {
            mask.paint(bounds, transform, color.opacity(pose.opacity), window, cx);
        }
    }
}

fn pose_transform(bounds: Bounds<Pixels>, pose: Pose, scale_factor: f32) -> TransformationMatrix {
    let center = bounds.center();
    TransformationMatrix::unit()
        .translate(center.scale(scale_factor))
        .rotate(radians(pose.rotation.to_radians()))
        .scale(size(pose.scale, pose.scale))
        .translate(center.scale(-scale_factor))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Arc;

    use super::*;

    #[test]
    fn masks_parse_and_have_distinct_stable_cache_keys_at_each_dpi() {
        let renderer = gpui::SvgRenderer::new(Arc::new(()));
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let masks = RingMasks::new(scale);
            assert!(masks.matches_scale(scale));
            assert!(!masks.matches_scale(scale + 0.25));
            let mut keys = HashSet::new();
            for mask in cached_layers(&masks) {
                assert!(keys.insert(mask.key.clone()));
                renderer.parse_svg(mask.data.as_bytes()).unwrap();
            }
            let rebuilt = RingMasks::new(scale);
            for (a, b) in cached_layers(&masks).zip(cached_layers(&rebuilt)) {
                assert_eq!(a.key, b.key);
                assert_eq!(a.data, b.data);
            }
            assert_eq!(masks.cancel, rebuilt.cancel);
        }
    }

    #[test]
    fn rasterized_wedges_match_slot_centers_and_have_antialiased_edges() {
        let renderer = gpui::SvgRenderer::new(Arc::new(()));
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let masks = RingMasks::new(scale);
            let geometry = Geometry::new(Bounds::new(
                Point::default(),
                size(px(WINDOW_SIZE), px(WINDOW_SIZE)),
            ));
            for slot in ActionRingSlot::ALL {
                let mask = &masks.slots[slot.index()][0];
                let parsed = renderer.parse_svg(mask.data.as_bytes()).unwrap();
                let image = renderer
                    .render_parsed(&parsed, gpui::SvgSize::ScaleFactor(scale))
                    .unwrap();
                let bytes = image.as_bytes(0).unwrap();
                let sample = |point| alpha_at(&image, mask.viewport, point);
                for candidate in ActionRingSlot::ALL {
                    let alpha = sample(geometry.content_center(candidate, Pose::REST));
                    assert_eq!(alpha, if candidate == slot { 255 } else { 0 });
                }
                assert_eq!(
                    sample(gpui::point(px(WINDOW_SIZE / 2.0), px(WINDOW_SIZE / 2.0))),
                    0
                );
                assert_eq!(sample(gpui::point(px(1.0), px(1.0))), 0);
                let edge_levels: HashSet<_> = bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|pixel| pixel[3])
                    .filter(|alpha| *alpha > 0 && *alpha < 255)
                    .collect();
                assert!(
                    edge_levels.len() > 16,
                    "edge coverage should not be binary or coarse"
                );
            }
        }
    }

    #[test]
    fn mask_animation_tracks_hit_geometry_at_each_dpi() {
        let bounds = Bounds::new(
            gpui::point(px(23.0), px(41.0)),
            size(px(WINDOW_SIZE), px(WINDOW_SIZE)),
        );
        let geometry = Geometry::new(bounds);
        for scale in [1.0, 1.25, 1.5, 2.0] {
            for pose in [
                Pose::REST,
                Pose {
                    scale: 0.67,
                    rotation: -9.0,
                    opacity: 0.5,
                },
            ] {
                let transform = pose_transform(bounds, pose, scale);
                for slot in ActionRingSlot::ALL {
                    let rest = geometry.content_center(slot, Pose::REST).map(|p| p * scale);
                    let painted = transform.apply(rest);
                    let expected = geometry.content_center(slot, pose).map(|p| p * scale);
                    assert!(f32::from(painted.x - expected.x).abs() < 0.001);
                    assert!(f32::from(painted.y - expected.y).abs() < 0.001);
                }
            }
        }
    }

    fn cached_layers(masks: &RingMasks) -> impl Iterator<Item = &Mask> {
        masks
            .slots
            .iter()
            .flatten()
            .chain(std::iter::once(&masks.guides))
            .chain(&masks.pointer)
    }

    #[test]
    fn decorations_keep_transparent_backgrounds_and_fractional_edge_coverage() {
        let renderer = gpui::SvgRenderer::new(Arc::new(()));
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let masks = RingMasks::new(scale);
            let decorations = std::iter::once(&masks.guides)
                .chain(&masks.pointer)
                .map(|mask| (&mask.data, mask.viewport))
                .chain(
                    masks
                        .cancel
                        .iter()
                        .map(|data| (data, square(super::super::geometry::CANCEL_SIZE))),
                );
            for (data, viewport) in decorations {
                let parsed = renderer.parse_svg(data.as_bytes()).unwrap();
                let image = renderer
                    .render_parsed(&parsed, gpui::SvgSize::ScaleFactor(scale))
                    .unwrap();
                let bytes = image.as_bytes(0).unwrap();
                assert_eq!(bytes[3], 0, "transparent corner must not introduce a frame");
                let alpha: Vec<_> = bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|pixel| pixel[3])
                    .collect();
                assert!(alpha.iter().any(|value| *value > 0));
                assert!(alpha.iter().any(|value| *value > 0 && *value < 255));
                if data == &masks.cancel[0] {
                    assert_eq!(alpha_at(&image, viewport, viewport.center()), 255);
                } else if data == &masks.cancel[1] {
                    assert_eq!(alpha_at(&image, viewport, viewport.center()), 0);
                } else {
                    assert_eq!(alpha_at(&image, viewport, square(WINDOW_SIZE).center()), 0);
                }
            }
            assert_eq!(masks.guides.data.matches("<circle ").count(), 3);
            assert_eq!(masks.guides.data.matches("<path ").count(), 88);
            assert_eq!(masks.guides.data.matches("stroke-dasharray").count(), 1);
        }
    }

    fn alpha_at(image: &gpui::RenderImage, viewport: Bounds<Pixels>, point: Point<Pixels>) -> u8 {
        if !viewport.contains(&point) {
            return 0;
        }
        let extent = image.size(0);
        let width = u16::try_from(extent.width.0).unwrap();
        let height = u16::try_from(extent.height.0).unwrap();
        let offset = point - viewport.origin;
        let x = f32::from(offset.x) / f32::from(viewport.size.width) * f32::from(width);
        let y = f32::from(offset.y) / f32::from(viewport.size.height) * f32::from(height);
        let x = (0..width).find(|v| f32::from(*v + 1) > x).unwrap();
        let y = (0..height).find(|v| f32::from(*v + 1) > y).unwrap();
        image.as_bytes(0).unwrap()[(usize::from(y) * usize::from(width) + usize::from(x)) * 4 + 3]
    }
    #[test]
    fn pointer_rotation_remains_continuous_across_cardinal_angles() {
        let bounds = Bounds::new(Point::default(), size(px(WINDOW_SIZE), px(WINDOW_SIZE)));
        let geometry = Geometry::new(bounds);
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let rest = geometry
                .content_center(ActionRingSlot::Top, Pose::REST)
                .map(|p| p * scale);
            for angle in [
                -0.01, 0.0, 0.01, 89.99, 90.0, 90.01, 179.99, 180.01, 359.99, 360.01,
            ] {
                let pose = Pose {
                    rotation: angle,
                    ..Pose::REST
                };
                let painted = pose_transform(bounds, pose, scale).apply(rest);
                let expected = geometry
                    .content_center(ActionRingSlot::Top, pose)
                    .map(|p| p * scale);
                assert!(f32::from(painted.x - expected.x).abs() < 0.001);
                assert!(f32::from(painted.y - expected.y).abs() < 0.001);
            }
        }
    }

    #[test]
    #[ignore = "manual cold SVG rasterization timing; no GPU or hardware required"]
    fn cold_rasterization_timing() {
        let renderer = gpui::SvgRenderer::new(Arc::new(()));
        let started = std::time::Instant::now();
        let masks = RingMasks::new(1.5);
        for mask in cached_layers(&masks) {
            let at = std::time::Instant::now();
            let svg = renderer.parse_svg(mask.data.as_bytes()).unwrap();
            let _ = renderer
                .render_parsed(&svg, gpui::SvgSize::ScaleFactor(1.5))
                .unwrap();
            eprintln!("{}: {:?}", mask.key, at.elapsed());
        }
        eprintln!("cold masks total: {:?}", started.elapsed());
    }

    #[test]
    fn cold_masks_do_not_rasterize_a_full_window_per_small_shape() {
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let masks = RingMasks::new(scale);
            let area = |bounds: Bounds<Pixels>| {
                f32::from(bounds.size.width) * f32::from(bounds.size.height)
            };
            let full = area(square(WINDOW_SIZE));
            for mask in masks.slots.iter().flatten() {
                assert!(area(mask.viewport) < full * 0.2);
            }
            for mask in &masks.pointer {
                assert!(area(mask.viewport) < full * 0.01);
            }
            // Group opacity allocates intermediate full-canvas surfaces. Stroke
            // opacity gives these non-overlapping guide groups the same weights.
            assert!(!masks.guides.data.contains(" opacity="));
        }
    }
}
