//! Bounded outline construction through the core map; no lens polynomial lives in the UI.
use super::mask_canvas::Placement;
use iced::{Point, widget::canvas::Path};
const MAX_DEPTH: u8 = 10;
const MAX_SEGMENTS: usize = 1024;
#[derive(Default)]
pub(crate) struct Outline {
    pub segments: Vec<(Point, Point)>,
    pub approximate: bool,
}
impl Outline {
    pub fn path(&self) -> Path {
        Path::new(|builder| {
            for (a, b) in &self.segments {
                builder.move_to(*a);
                builder.line_to(*b);
            }
        })
    }
}
/// Clip a chord before evaluating the map, so no outside content point reaches a warp.
fn clip(mut a: (f64, f64), mut b: (f64, f64), max: (f64, f64)) -> Option<((f64, f64), (f64, f64))> {
    let d = (b.0 - a.0, b.1 - a.1);
    let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
    for (p, q) in [
        (-d.0, a.0),
        (d.0, max.0 - a.0),
        (-d.1, a.1),
        (d.1, max.1 - a.1),
    ] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
            continue;
        }
        let t = q / p;
        if p < 0.0 {
            lo = lo.max(t);
        } else {
            hi = hi.min(t);
        }
        if lo > hi {
            return None;
        }
    }
    b = (a.0 + d.0 * hi, a.1 + d.1 * hi);
    a = (a.0 + d.0 * lo, a.1 + d.1 * lo);
    Some((a, b))
}
fn same_outside(points: &[(f64, f64)], max: (f64, f64)) -> bool {
    points.iter().all(|p| p.0 < 0.0)
        || points.iter().all(|p| p.1 < 0.0)
        || points.iter().all(|p| p.0 > max.0)
        || points.iter().all(|p| p.1 > max.1)
}
fn deviation(p: Point, a: Point, b: Point) -> f64 {
    let (dx, dy) = ((b.x - a.x) as f64, (b.y - a.y) as f64);
    let (px, py) = ((p.x - a.x) as f64, (p.y - a.y) as f64);
    let den = dx * dx + dy * dy;
    if den == 0.0 {
        return px.hypot(py);
    }
    let t = ((px * dx + py * dy) / den).clamp(0.0, 1.0);
    (px - t * dx).hypot(py - t * dy)
}
pub(crate) fn curve(
    placement: &Placement,
    point: impl Fn(f64) -> (f64, f64),
    seeds: usize,
    dashed: bool,
) -> Outline {
    struct Build<'a, F> {
        placement: &'a Placement,
        point: F,
        outline: Outline,
        leaves: usize,
    }
    impl<F: Fn(f64) -> (f64, f64)> Build<'_, F> {
        fn segment(&mut self, t0: f64, t1: f64, depth: u8) {
            if self.leaves >= MAX_SEGMENTS {
                self.outline.approximate = true;
                return;
            }
            let tm = (t0 + t1) * 0.5;
            let content = [
                (self.point)(t0),
                (self.point)((t0 + tm) * 0.5),
                (self.point)(tm),
                (self.point)((tm + t1) * 0.5),
                (self.point)(t1),
            ];
            if same_outside(&content, self.placement.map.domain()) {
                return;
            }
            let mapped = content.map(|p| self.placement.canvas_point(p.0, p.1));
            let within = if let [Some(a), Some(q), Some(m), Some(r), Some(b)] = mapped {
                [q, m, r]
                    .iter()
                    .all(|p| deviation(*p, a, b) * self.placement.scale_factor as f64 <= 0.25)
            } else {
                false
            };
            if !within && depth < MAX_DEPTH {
                self.segment(t0, tm, depth + 1);
                self.segment(tm, t1, depth + 1);
                return;
            }
            self.leaves += 1;
            if !within {
                self.outline.approximate = true;
            }
            if let Some((a, b)) = clip(content[0], content[4], self.placement.map.domain())
                && let (Some(a), Some(b)) = (
                    self.placement.canvas_point(a.0, a.1),
                    self.placement.canvas_point(b.0, b.1),
                )
            {
                self.outline.segments.push((a, b));
            }
        }
    }
    let mut build = Build {
        placement,
        point,
        outline: Outline::default(),
        leaves: 0,
    };
    for i in 0..seeds {
        if !dashed || i % 2 == 0 {
            build.segment(i as f64 / seeds as f64, (i + 1) as f64 / seeds as f64, 0);
        }
    }
    build.outline
}
pub(crate) fn line(placement: &Placement, from: (f64, f64), to: (f64, f64)) -> Outline {
    let Some((a, b)) = clip(from, to, placement.map.domain()) else {
        return Outline::default();
    };
    curve(
        placement,
        |t| (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t),
        1,
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{mask_draft::ContentMap, view::canvas_view::CanvasView};
    #[test]
    fn perspective_curves_and_pointer_use_the_core_map_at_fit_and_percent() {
        let geometry = crate::state::testing::perspective_mapping();
        let map = ContentMap::new(&geometry).unwrap();
        for view in [
            CanvasView::fit((6000.0, 4000.0), iced::Size::new(1200.0, 800.0)).unwrap(),
            CanvasView::percent(100.0, 2.0).unwrap(),
        ] {
            let p = Placement {
                map: map.clone(),
                view,
                scale_factor: 2.0,
            };
            for content in [(0.3, 0.4), (0.5, 0.5), (0.65, 0.6)] {
                let screen = p.canvas_point(content.0, content.1).unwrap();
                let back = p.content_point(screen).unwrap();
                assert!((back.0 - content.0).abs() < 2e-7 && (back.1 - content.1).abs() < 2e-7);
            }
            let curve = curve(
                &p,
                |t| {
                    let a = t * std::f64::consts::TAU;
                    (0.5 + 0.12 * a.cos(), 0.5 + 0.18 * a.sin())
                },
                32,
                false,
            );
            assert!(!curve.approximate);
            assert!(curve.segments.len() <= 1024);
            assert!(curve.segments.len() >= 32);
        }
    }
    #[test]
    fn clipped_line_crosses_domain_and_curves_obey_physical_error() {
        let id = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        let p = Placement {
            map: ContentMap::from_affine((6000, 4000), (6000, 4000), id, id).unwrap(),
            view: CanvasView::percent(100.0, 2.0).unwrap(),
            scale_factor: 2.0,
        };
        let line = line(&p, (-1.0, 0.5), (2.0, 0.5));
        assert_eq!(line.segments.len(), 1);
        assert_eq!(line.segments[0].0.x, 0.0);
        assert!(!line.approximate);
        let circle = curve(
            &p,
            |t| {
                let a = t * std::f64::consts::TAU;
                (0.5 + 0.25 * a.cos(), 0.5 + 0.375 * a.sin())
            },
            32,
            false,
        );
        assert!(circle.segments.len() > 32 && circle.segments.len() <= 1024);
        assert!(!circle.approximate);
        for (a, b) in circle.segments {
            let radius = 750.0;
            let distance = deviation(Point::new(1500.0, 1000.0), a, b);
            assert!((radius - distance) * 2.0 < 0.26);
        }
    }
    #[test]
    fn subdivision_has_a_hard_budget() {
        let id = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        let p = Placement {
            map: ContentMap::from_affine((6000, 4000), (6000, 4000), id, id).unwrap(),
            view: CanvasView::percent(10000.0, 1.0).unwrap(),
            scale_factor: 1.0,
        };
        let outline = curve(
            &p,
            |t| {
                let a = t * std::f64::consts::TAU;
                (0.5 + 0.25 * a.cos(), 0.5 + 0.375 * a.sin())
            },
            32,
            false,
        );
        assert!(outline.segments.len() <= 1024);
        assert!(outline.approximate);
    }

    #[test]
    fn mask_outline_outside_content_is_clipped_under_perspective() {
        let map = crate::state::testing::nonlinear_mapping(
            crate::state::testing::TestOrientation::NEUTRAL,
            100,
            100,
        );
        let p = Placement {
            map: ContentMap::new(&map).unwrap(),
            view: CanvasView::percent(100.0, 2.0).unwrap(),
            scale_factor: 2.0,
        };
        // The centre and much of this legal radial mask are outside the picture, including the
        // extended projective pole. Only the arc crossing content is allowed to reach the map.
        assert!(p.canvas_point(-1.0, -1.0).is_none());
        let outline = curve(
            &p,
            |t| {
                let angle = t * std::f64::consts::TAU;
                (-1.0 + 2.2 * angle.cos(), -1.0 + 2.2 * angle.sin())
            },
            32,
            false,
        );
        assert!(!outline.segments.is_empty());
        assert!(outline.segments.len() <= 1024);
        for (a, b) in outline.segments {
            for point in [a, b] {
                let (x, y) = p.view.stage_point(point);
                assert!(x.is_finite() && y.is_finite());
                assert!((0.0..=6000.001).contains(&x) && (0.0..=4000.001).contains(&y));
                let content = map
                    .to_content(x.clamp(0.0, 6000.0), y.clamp(0.0, 4000.0))
                    .unwrap();
                assert!(
                    (0.0..=6000.001).contains(&content.0) && (0.0..=4000.001).contains(&content.1)
                );
            }
        }
    }

    #[test]
    fn outline_line_follows_warp_within_quarter_pixel() {
        for (h, v) in [(0, 0), (40, -25)] {
            let geometry = crate::state::testing::nonlinear_mapping(
                crate::state::testing::TestOrientation::NEUTRAL,
                h,
                v,
            );
            let p = Placement {
                map: ContentMap::new(&geometry).unwrap(),
                view: CanvasView::percent(100.0, 2.0).unwrap(),
                scale_factor: 2.0,
            };
            let (a, b) = ((0.15, 0.32), (0.85, 0.62));
            let outline = line(&p, a, b);
            assert!(!outline.approximate);
            assert!(outline.segments.len() > 1 && outline.segments.len() <= 1024);
            for i in 0..=8192 {
                let t = i as f64 / 8192.0;
                let Some(point) = p.canvas_point(a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
                else {
                    continue;
                };
                let error = outline
                    .segments
                    .iter()
                    .map(|(a, b)| deviation(point, *a, *b))
                    .fold(f64::INFINITY, f64::min)
                    * 2.0;
                assert!(error <= 0.25, "{h},{v} t={t}: {error} physical px");
            }
        }
    }

    #[test]
    fn outline_segment_cap_reports_approximate_under_warp() {
        let geometry = crate::state::testing::nonlinear_mapping(
            crate::state::testing::TestOrientation::NEUTRAL,
            40,
            -25,
        );
        let p = Placement {
            map: ContentMap::new(&geometry).unwrap(),
            view: CanvasView::percent(10000.0, 1.0).unwrap(),
            scale_factor: 1.0,
        };
        let outline = curve(
            &p,
            |t| {
                let angle = t * std::f64::consts::TAU;
                (0.5 + 0.15 * angle.cos(), 0.5 + 0.2 * angle.sin())
            },
            32,
            false,
        );
        assert_eq!(outline.segments.len(), 1024);
        assert!(outline.approximate);
    }

    #[test]
    fn pick_positions_agree_core_ui_api() {
        for mirror in [false, true] {
            for turns in 0..4 {
                let geometry = crate::state::testing::nonlinear_mapping(
                    crate::state::testing::TestOrientation { mirror, turns },
                    40,
                    -25,
                );
                let map = ContentMap::new(&geometry).unwrap();
                let (w, h) = map.output();
                let fit = CanvasView::fit((w, h), iced::Size::new(1200.0, 850.0)).unwrap();
                let percent = CanvasView::percent(100.0, 2.0).unwrap();
                let pan = CanvasView {
                    origin: iced::Vector::new(-313.0, -97.0),
                    ..percent
                };
                for view in [fit, percent, pan] {
                    let p = Placement {
                        map: map.clone(),
                        view,
                        scale_factor: 2.0,
                    };
                    for screen in [Point::new(85.125, 121.5), Point::new(301.25, 204.75)] {
                        let (x, y) = view.stage_point(screen);
                        if !map.visible(x, y) {
                            continue;
                        }
                        let continuous = geometry.to_content(x, y).unwrap();
                        let ui = p.content_point(screen).unwrap();
                        assert!((ui.0 * 6000.0 - continuous.0).abs() < 1e-9);
                        assert!((ui.1 * 4000.0 - continuous.1).abs() < 1e-9);
                        // A pixel query maps its centre before flooring. Quantising the pointer
                        // first would choose a different input at this nonlinear stage.
                        let centre = (x.floor() + 0.5, y.floor() + 0.5);
                        let core = geometry.to_content(centre.0, centre.1).unwrap();
                        let ui = map.to_content(centre.0, centre.1).unwrap();
                        assert_eq!(
                            (core.0.floor(), core.1.floor()),
                            ((ui.0 * 6000.0).floor(), (ui.1 * 4000.0).floor())
                        );
                    }
                }
            }
        }
    }
}
