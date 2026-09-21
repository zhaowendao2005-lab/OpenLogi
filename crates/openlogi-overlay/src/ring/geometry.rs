//! One coordinate system for the painted wedges and their pointer targets.

use std::fmt::Write as _;

use gpui::{Bounds, Pixels, Point, point, px};
use openlogi_core::binding::ActionRingSlot;

pub(super) const DESIGN_SIZE: f32 = 520.0;
pub(super) const CANCEL_SIZE: f32 = 48.0;
const INNER_RADIUS: f32 = 88.0;
const OUTER_RADIUS: f32 = 205.0;
const CONTENT_RADIUS: f32 = 145.0;
const HALF_SECTOR: f32 = 21.0;

#[derive(Clone, Copy, Debug)]
pub(super) struct Pose {
    pub scale: f32,
    pub rotation: f32,
    pub opacity: f32,
}

impl Pose {
    pub const REST: Self = Self {
        scale: 1.0,
        rotation: 0.0,
        opacity: 1.0,
    };
}

#[derive(Clone, Copy)]
pub(super) struct Geometry {
    center: Point<Pixels>,
    scale: f32,
}

impl Geometry {
    pub fn new(bounds: Bounds<Pixels>) -> Self {
        Self {
            center: bounds.center(),
            scale: f32::from(bounds.size.width.min(bounds.size.height)) / DESIGN_SIZE,
        }
    }

    pub fn length(self, value: f32) -> Pixels {
        px(value * self.scale)
    }

    pub fn point(self, radius: f32, angle: f32, pose: Pose) -> Point<Pixels> {
        let radians = (angle + pose.rotation).to_radians();
        self.center
            + point(
                self.length(radius * pose.scale * radians.sin()),
                self.length(-radius * pose.scale * radians.cos()),
            )
    }

    pub fn content_center(self, slot: ActionRingSlot, pose: Pose) -> Point<Pixels> {
        self.point(CONTENT_RADIUS, slot_angle(slot), pose)
    }

    /// The stable ring annulus excludes the central cancel target and outside.
    pub fn in_ring(self, position: Point<Pixels>) -> bool {
        let offset = position - self.center;
        let radius = f32::from(offset.x).hypot(f32::from(offset.y));
        radius >= CANCEL_SIZE / 2.0 && radius <= OUTER_RADIUS * self.scale
    }

    pub fn contains(self, position: Point<Pixels>, slot: ActionRingSlot, pose: Pose) -> bool {
        if pose.opacity <= 0.01 || pose.scale <= 0.0 || self.scale <= 0.0 {
            return false;
        }
        let offset = position - self.center;
        let x = f32::from(offset.x) / (self.scale * pose.scale);
        let y = f32::from(offset.y) / (self.scale * pose.scale);
        let radius = x.hypot(y);
        let angle = x.atan2(-y).to_degrees() - pose.rotation;
        (INNER_RADIUS..=OUTER_RADIUS).contains(&radius)
            && shortest_turn(slot_angle(slot), angle).abs() <= HALF_SECTOR
    }

    /// Only rasterize occupied pixels. Padding preserves the stroke and its AA
    /// fringe; outward DPI alignment keeps the cropped mask on the same grid.
    pub fn wedge_viewport(self, slot: ActionRingSlot, grid: PixelGrid) -> Bounds<Pixels> {
        let angle = slot_angle(slot);
        let points = [INNER_RADIUS, OUTER_RADIUS].into_iter().flat_map(|radius| {
            [-HALF_SECTOR, 0.0, HALF_SECTOR]
                .map(|offset| self.point(radius, angle + offset, Pose::REST))
        });
        grid.enclose(points)
    }

    pub fn pointer_viewport(self, grid: PixelGrid) -> Bounds<Pixels> {
        grid.enclose(
            [
                self.point(231.0, -1.5, Pose::REST),
                self.point(231.0, 1.5, Pose::REST),
                self.point(209.0, 0.0, Pose::REST),
            ]
            .into_iter(),
        )
    }
    /// Serialize the same radii and angular boundaries used by hit testing.
    /// Animation transforms this rest-pose mask rather than rebuilding its SVG.
    pub fn wedge_svg(
        self,
        slot: ActionRingSlot,
        border_width: Option<Pixels>,
        viewport: Bounds<Pixels>,
    ) -> String {
        let start = slot_angle(slot) - HALF_SECTOR;
        let end = slot_angle(slot) + HALF_SECTOR;
        let a = self.point(OUTER_RADIUS, start, Pose::REST);
        let b = self.point(OUTER_RADIUS, end, Pose::REST);
        let c = self.point(INNER_RADIUS, end, Pose::REST);
        let d = self.point(INNER_RADIUS, start, Pose::REST);
        let outer = f32::from(self.length(OUTER_RADIUS));
        let inner = f32::from(self.length(INNER_RADIUS));
        let style = border_width.map_or_else(
            || "fill=\"white\"".to_owned(),
            |width| {
                format!(
                    "fill=\"none\" stroke=\"white\" stroke-width=\"{}\"",
                    f32::from(width)
                )
            },
        );
        svg_document(
            viewport,
            &format!(
                r#"<path {style} d="M {} {} A {outer} {outer} 0 0 1 {} {} L {} {} A {inner} {inner} 0 0 0 {} {} Z"/>"#,
                f32::from(a.x),
                f32::from(a.y),
                f32::from(b.x),
                f32::from(b.y),
                f32::from(c.x),
                f32::from(c.y),
                f32::from(d.x),
                f32::from(d.y),
            ),
        )
    }

    /// A single cached layer; opacity groups retain the reference's hierarchy.
    pub fn guides_svg(self, grid: PixelGrid) -> String {
        let mut body = String::new();
        let cx = f32::from(self.center.x);
        let cy = f32::from(self.center.y);
        let width = f32::from(grid.stroke_width(self.length(0.65)));
        body.push_str(r#"<g fill="none" stroke="white" stroke-opacity="0.6">"#);
        for (radius, dashed) in [(235.0, false), (216.0, true), (78.0, false)] {
            let radius = f32::from(self.length(radius));
            let dash = if dashed {
                format!(
                    r#" stroke-dasharray="{} {}""#,
                    f32::from(self.length(2.0)),
                    f32::from(self.length(5.0))
                )
            } else {
                String::new()
            };
            // SVG circles start at three o'clock; the reference starts at twelve.
            let _ = write!(
                body,
                r#"<circle cx="{cx}" cy="{cy}" r="{radius}" stroke-width="{width}"{dash} transform="rotate(-90 {cx} {cy})"/>"#
            );
        }
        body.push_str("</g>");
        let width = grid.stroke_width(self.length(1.0));
        let _ = write!(
            body,
            r#"<g fill="none" stroke="white" stroke-width="{}" stroke-opacity="0.85">"#,
            f32::from(width)
        );
        for i in 0_u16..80 {
            let major = i % 10 == 0;
            let angle = f32::from(i) * 4.5;
            let (start, end) = grid.line(
                self.point(if major { 239.0 } else { 240.0 }, angle, Pose::REST),
                self.point(if major { 248.0 } else { 244.0 }, angle, Pose::REST),
                width,
            );
            svg_line(&mut body, start, end);
        }
        body.push_str("</g>");
        let width = f32::from(grid.stroke_width(self.length(0.7)));
        let _ = write!(
            body,
            r#"<g fill="none" stroke="white" stroke-width="{width}" stroke-opacity="0.45">"#
        );
        for slot in ActionRingSlot::ALL {
            let angle = slot_angle(slot) + 22.5;
            svg_line(
                &mut body,
                self.point(80.0, angle, Pose::REST),
                self.point(230.0, angle, Pose::REST),
            );
        }
        body.push_str("</g>");
        svg_document(square(f32::from(self.length(DESIGN_SIZE))), &body)
    }

    /// Rotate the completed masks, never snap a moving endpoint; that would
    /// introduce a discontinuity at cardinal angles.
    pub fn pointer_svgs(self, grid: PixelGrid, viewport: Bounds<Pixels>) -> [String; 2] {
        let a = self.point(231.0, -1.5, Pose::REST);
        let b = self.point(231.0, 1.5, Pose::REST);
        let tip = self.point(223.0, 0.0, Pose::REST);
        let triangle = svg_document(
            viewport,
            &format!(
                r#"<path fill="white" d="M {} {} L {} {} L {} {} Z"/>"#,
                f32::from(a.x),
                f32::from(a.y),
                f32::from(b.x),
                f32::from(b.y),
                f32::from(tip.x),
                f32::from(tip.y),
            ),
        );
        let width = f32::from(grid.stroke_width(self.length(1.0)));
        let mut stem = format!(r#"<g fill="none" stroke="white" stroke-width="{width}">"#);
        svg_line(
            &mut stem,
            self.point(217.0, 0.0, Pose::REST),
            self.point(209.0, 0.0, Pose::REST),
        );
        stem.push_str("</g>");
        [triangle, svg_document(viewport, &stem)]
    }
}

/// The circle fills the existing target, with its border drawn inside it.
pub(super) fn cancel_svgs() -> [String; 2] {
    let center = CANCEL_SIZE / 2.0;
    [
        svg_document(
            square(CANCEL_SIZE),
            &format!(r#"<circle cx="{center}" cy="{center}" r="{center}" fill="white"/>"#),
        ),
        svg_document(
            square(CANCEL_SIZE),
            &format!(
                r#"<circle cx="{center}" cy="{center}" r="{}" fill="none" stroke="white" stroke-width="1"/>"#,
                center - 0.5
            ),
        ),
    ]
}

pub(super) fn square(size: f32) -> Bounds<Pixels> {
    Bounds::new(Point::default(), gpui::size(px(size), px(size)))
}

fn svg_document(bounds: Bounds<Pixels>, body: &str) -> String {
    let (x, y, width, height) = (
        f32::from(bounds.origin.x),
        f32::from(bounds.origin.y),
        f32::from(bounds.size.width),
        f32::from(bounds.size.height),
    );
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="{x} {y} {width} {height}">{body}</svg>"#
    )
}
fn svg_line(body: &mut String, start: Point<Pixels>, end: Point<Pixels>) {
    // Writing to String is infallible. Generate strings only on cache creation.
    let _ = write!(
        body,
        r#"<path d="M {} {} L {} {}"/>"#,
        f32::from(start.x),
        f32::from(start.y),
        f32::from(end.x),
        f32::from(end.y)
    );
}

pub(super) fn slot_angle(slot: ActionRingSlot) -> f32 {
    let (x, y) = slot.unit_offset();
    x.atan2(-y).to_degrees().rem_euclid(360.0)
}

pub(super) fn shortest_turn(from: f32, to: f32) -> f32 {
    (to - from + 180.0).rem_euclid(360.0) - 180.0
}

/// Physical-pixel policy for strokes and stationary content, independent of
/// the reference drawing scale. Curves and moving content retain fractional coordinates.
#[derive(Clone, Copy)]
pub(super) struct PixelGrid {
    scale_factor: f32,
}

impl PixelGrid {
    pub fn new(scale_factor: f32) -> Self {
        debug_assert!(scale_factor.is_finite() && scale_factor > 0.0);
        Self { scale_factor }
    }

    fn enclose(self, points: impl Iterator<Item = Point<Pixels>> + Clone) -> Bounds<Pixels> {
        let minimum = points
            .clone()
            .fold(point(px(f32::INFINITY), px(f32::INFINITY)), |a, b| {
                point(a.x.min(b.x), a.y.min(b.y))
            });
        let maximum = points.fold(
            point(px(f32::NEG_INFINITY), px(f32::NEG_INFINITY)),
            |a, b| point(a.x.max(b.x), a.y.max(b.y)),
        );
        let origin = minimum
            .map(|p| px(((f32::from(p) * self.scale_factor).floor() - 2.0) / self.scale_factor));
        let end = maximum
            .map(|p| px(((f32::from(p) * self.scale_factor).ceil() + 2.0) / self.scale_factor));
        Bounds::new(origin, gpui::size(end.x - origin.x, end.y - origin.y))
    }
    pub fn stroke_width(self, logical: Pixels) -> Pixels {
        px((f32::from(logical) * self.scale_factor).round().max(1.0) / self.scale_factor)
    }

    pub fn snap(self, position: Point<Pixels>) -> Point<Pixels> {
        position.map(|coordinate| {
            px((f32::from(coordinate) * self.scale_factor).round() / self.scale_factor)
        })
    }

    /// Snap only axis-aligned lines. Odd physical widths need half-pixel
    /// centres; diagonal endpoints must not be independently distorted.
    pub fn line(
        self,
        start: Point<Pixels>,
        end: Point<Pixels>,
        width: Pixels,
    ) -> (Point<Pixels>, Point<Pixels>) {
        let center = |coordinate: Pixels| {
            let half_width = f32::from(width) * self.scale_factor / 2.0;
            px(
                ((f32::from(coordinate) * self.scale_factor - half_width).round() + half_width)
                    / self.scale_factor,
            )
        };
        // Polar coordinates carry tiny sin/cos error at the cardinal directions.
        let vertical = f32::from(start.x - end.x).abs() * self.scale_factor < 0.001;
        let horizontal = f32::from(start.y - end.y).abs() * self.scale_factor < 0.001;
        if vertical {
            let x = center(start.x);
            (point(x, self.snap(start).y), point(x, self.snap(end).y))
        } else if horizontal {
            let y = center(start.y);
            (point(self.snap(start).x, y), point(self.snap(end).x, y))
        } else {
            (start, end)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::size;

    fn geometry() -> Geometry {
        Geometry::new(Bounds::new(
            point(px(27.0), px(43.0)),
            size(px(360.0), px(360.0)),
        ))
    }

    #[test]
    fn reference_hairlines_have_whole_physical_pixel_width_at_each_dpi() {
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let grid = PixelGrid::new(scale);
            for design_width in [0.65, 0.7, 0.8, 1.0, 3.0] {
                let width = grid.stroke_width(geometry().length(design_width));
                let physical = f32::from(width) * scale;
                assert!(physical >= 1.0);
                assert!((physical - physical.round()).abs() < 0.0001);
            }
        }
    }

    #[test]
    fn snapped_anchors_and_cardinal_stroke_edges_land_on_device_pixels() {
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let grid = PixelGrid::new(scale);
            let snapped = grid.snap(point(px(12.37), px(24.61)));
            for coordinate in [snapped.x, snapped.y] {
                let physical = f32::from(coordinate) * scale;
                assert!((physical - physical.round()).abs() < 0.0001);
            }
            let width = grid.stroke_width(px(0.55));
            for (start, end) in [
                (point(px(12.37), px(10.2)), point(px(12.37), px(15.7))),
                (point(px(10.2), px(12.37)), point(px(15.7), px(12.37))),
            ] {
                let (a, b) = grid.line(start, end, width);
                let coordinate = if (f32::from(a.x - b.x)).abs() < 0.001 {
                    a.x
                } else {
                    a.y
                };
                for edge in [coordinate - width / 2.0, coordinate + width / 2.0] {
                    let physical = f32::from(edge) * scale;
                    assert!((physical - physical.round()).abs() < 0.0001);
                }
            }
        }
    }

    #[test]
    fn diagonal_strokes_keep_their_exact_geometry() {
        let start = point(px(13.37), px(21.61));
        let end = point(px(19.17), px(27.83));
        let grid = PixelGrid::new(1.5);
        assert_eq!(
            grid.line(start, end, grid.stroke_width(px(0.55))),
            (start, end)
        );
    }

    #[test]
    fn all_eight_centers_hit_only_their_slot() {
        let geometry = geometry();
        for slot in ActionRingSlot::ALL {
            let center = geometry.content_center(slot, Pose::REST);
            for candidate in ActionRingSlot::ALL {
                assert_eq!(
                    geometry.contains(center, candidate, Pose::REST),
                    candidate == slot
                );
            }
        }
    }

    #[test]
    fn hole_outer_margin_and_gaps_are_not_actions() {
        let geometry = geometry();
        for radius in [0.0, 30.0, 87.9, 205.1, 245.0] {
            let position = geometry.point(radius, 0.0, Pose::REST);
            assert!(
                ActionRingSlot::ALL
                    .into_iter()
                    .all(|slot| !geometry.contains(position, slot, Pose::REST))
            );
        }
        for slot in ActionRingSlot::ALL {
            let gap = geometry.point(145.0, slot_angle(slot) + 22.5, Pose::REST);
            assert!(
                ActionRingSlot::ALL
                    .into_iter()
                    .all(|candidate| !geometry.contains(gap, candidate, Pose::REST))
            );
            for angle in [-20.99, 20.99] {
                assert!(geometry.contains(
                    geometry.point(145.0, slot_angle(slot) + angle, Pose::REST),
                    slot,
                    Pose::REST
                ));
            }
        }
    }

    #[test]
    fn translated_scaled_and_animated_geometry_has_matching_targets() {
        for edge in [180.0, 360.0, 720.0] {
            let geometry = Geometry::new(Bounds::new(
                point(px(90.0), px(120.0)),
                size(px(edge), px(edge)),
            ));
            let pose = Pose {
                scale: 0.74,
                rotation: -7.0,
                opacity: 0.5,
            };
            for slot in ActionRingSlot::ALL {
                assert!(geometry.contains(geometry.content_center(slot, pose), slot, pose));
                assert!(!geometry.contains(
                    geometry.point(210.0, slot_angle(slot), pose),
                    slot,
                    pose
                ));
            }
        }
    }

    #[test]
    fn invisible_sectors_cannot_activate() {
        let geometry = geometry();
        assert!(!geometry.contains(
            geometry.content_center(ActionRingSlot::Top, Pose::REST),
            ActionRingSlot::Top,
            Pose {
                opacity: 0.0,
                ..Pose::REST
            }
        ));
    }
}
