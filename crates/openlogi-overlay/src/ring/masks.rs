//! Fixed SVG masks for the ring's curved edges. GPUI owns rasterization and its
//! DPI-sized atlas; only color and a bounded pose transform change per frame.

use gpui::{
    App, Bounds, Hsla, Pixels, Point, SharedString, TransformationMatrix, Window, px, radians, size,
};
use openlogi_core::binding::ActionRingSlot;

use super::WINDOW_SIZE;
use super::geometry::{Geometry, PixelGrid, Pose};

struct Mask {
    key: SharedString,
    data: String,
}

pub(super) struct WedgeMasks {
    scale_factor: f32,
    slots: [[Mask; 2]; 8],
}

impl WedgeMasks {
    pub fn new(scale_factor: f32) -> Self {
        let geometry = Geometry::new(Bounds::new(
            Point::default(),
            size(px(WINDOW_SIZE), px(WINDOW_SIZE)),
        ));
        let width = PixelGrid::new(scale_factor).stroke_width(geometry.length(0.8));
        Self {
            scale_factor,
            slots: ActionRingSlot::ALL.map(|slot| {
                [None, Some(width)].map(|stroke| Mask {
                    // Namespaced identities stay constant through animation. The
                    // only variable SVG input, stroke width, is part of the key.
                    key: format!(
                        "__openlogi_ring_wedge_{}_{}",
                        slot.index(),
                        stroke.map_or(0, |width| f32::from(width).to_bits()),
                    )
                    .into(),
                    data: geometry.wedge_svg(slot, stroke),
                })
            }),
        }
    }

    pub fn matches_scale(&self, scale_factor: f32) -> bool {
        self.scale_factor.to_bits() == scale_factor.to_bits()
    }

    pub fn paint(
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
            if let Err(error) = window.paint_svg(
                bounds,
                mask.key.clone(),
                Some(mask.data.as_bytes()),
                transform,
                color.opacity(pose.opacity),
                cx,
            ) {
                tracing::warn!(?error, "could not paint Actions Ring SVG mask");
            }
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
            let masks = WedgeMasks::new(scale);
            assert!(masks.matches_scale(scale));
            assert!(!masks.matches_scale(scale + 0.25));
            let mut keys = HashSet::new();
            for mask in masks.slots.iter().flatten() {
                assert!(keys.insert(mask.key.clone()));
                renderer.parse_svg(mask.data.as_bytes()).unwrap();
            }
            let rebuilt = WedgeMasks::new(scale);
            for (a, b) in masks
                .slots
                .iter()
                .flatten()
                .zip(rebuilt.slots.iter().flatten())
            {
                assert_eq!(a.key, b.key);
                assert_eq!(a.data, b.data);
            }
        }
    }

    #[test]
    fn rasterized_wedges_match_slot_centers_and_have_antialiased_edges() {
        let renderer = gpui::SvgRenderer::new(Arc::new(()));
        for (scale, physical_size) in [(1.0, 360_u16), (1.25, 450), (1.5, 540), (2.0, 720)] {
            let masks = WedgeMasks::new(scale);
            let geometry = Geometry::new(Bounds::new(
                Point::default(),
                size(px(WINDOW_SIZE), px(WINDOW_SIZE)),
            ));
            for slot in ActionRingSlot::ALL {
                let parsed = renderer
                    .parse_svg(masks.slots[slot.index()][0].data.as_bytes())
                    .unwrap();
                let extent = gpui::DevicePixels(i32::from(physical_size));
                let image = renderer
                    .render_parsed(&parsed, gpui::SvgSize::ExactSize(size(extent, extent)))
                    .unwrap();
                let bytes = image.as_bytes(0).unwrap();
                let sample = |point: Point<Pixels>| {
                    // Bounded test coordinates are inside the 360-logical-pixel canvas.
                    let x = f32::from(point.x) * scale;
                    let y = f32::from(point.y) * scale;
                    let x = (0..physical_size).find(|v| f32::from(*v + 1) > x).unwrap();
                    let y = (0..physical_size).find(|v| f32::from(*v + 1) > y).unwrap();
                    bytes[(usize::from(y) * usize::from(physical_size) + usize::from(x)) * 4 + 3]
                };
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
}
