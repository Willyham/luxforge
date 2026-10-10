//! One painted path as the pointer draws it: the capture every brush consumer shares.
//!
//! The path is held on the host's own grid through [`luxforge_core::path::PathCapture`], which
//! checks and snaps each position once, when it is painted, and refuses the whole gesture at its
//! bound. It is **decimated only when it is posted**, at the host's own tolerance for the radius it
//! is posted with, which is idempotent — so what the desktop sends and what an agent sends arrive at
//! the same stored stroke. One decimation is cached per held path and radius, so request fields and
//! evidence summaries never repeat that work for the same input, and a position that snaps into the
//! cell before it keeps it.
//!
//! Nothing here knows which consumer paints: a mask stroke and a declared `brush-mask` path capture
//! through exactly this type, and differ only in the settings and the action their binding names
//! ([`super::BrushBinding`]).
use std::{cell::RefCell, sync::Arc};

/// The path in flight. Cheap to clone, bounded by the host's capture bound, and free of pixels.
#[derive(Clone, Debug, Default)]
pub(crate) struct StrokeCapture {
    /// The path on the stored grid, snapped one position at a time as it is painted.
    grid: luxforge_core::path::PathCapture,
    /// The first and last positions painted and how many were, for the pin a path is selected by,
    /// a straight stroke's start and evidence of what the pointer did. Positions in one grid cell
    /// cost only the count.
    first: Option<[f64; 2]>,
    last: Option<[f64; 2]>,
    captured: usize,
    /// One whole-path reduction per capture and radius, shared by fields and summaries. At most the
    /// capture's own bounded number of positions; no pixels or source are retained.
    points: RefCell<Option<CachedPoints>>,
    /// The pointer is down: moves extend the path, and a move with it up is not painting.
    painting: bool,
}

impl PartialEq for StrokeCapture {
    fn eq(&self, other: &Self) -> bool {
        self.grid == other.grid
            && self.first == other.first
            && self.last == other.last
            && self.captured == other.captured
            && self.painting == other.painting
    }
}

#[derive(Clone, Debug, PartialEq)]
struct CachedPoints {
    radius: u64,
    /// Shared, so building the fields reads the path without copying it.
    points: Arc<[[f64; 2]]>,
}

/// How many times this thread decimated a captured path and serialized a posted one, for the tests
/// that hold a stroke's moves to the sends they make.
#[cfg(test)]
pub(crate) mod counts {
    use std::cell::Cell;

    thread_local! {
        static DECIMATED: Cell<u64> = const { Cell::new(0) };
        static SERIALIZED: Cell<u64> = const { Cell::new(0) };
    }

    pub(super) fn decimated() {
        DECIMATED.with(|count| count.set(count.get() + 1));
    }

    pub(crate) fn serialized() {
        SERIALIZED.with(|count| count.set(count.get() + 1));
    }

    /// The decimations and the posted paths serialized on this thread since it last asked.
    pub(crate) fn take() -> (u64, u64) {
        (
            DECIMATED.with(|count| count.replace(0)),
            SERIALIZED.with(|count| count.replace(0)),
        )
    }
}

impl StrokeCapture {
    /// A path that was never pressed: nothing drawn and nothing to post.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// A finished path of these positions, as a click (one position) or a straight stroke (two)
    /// produces it: pressed at the first, painted through the rest and released.
    pub(crate) fn of(points: &[[f64; 2]]) -> Self {
        let mut capture = Self::new();
        if let Some((first, rest)) = points.split_first() {
            capture.press((first[0], first[1]));
            for point in rest {
                capture.paint((point[0], point[1]));
            }
            capture.release();
        }
        capture
    }

    /// The pointer went down: this path starts here, replacing whatever was held.
    pub(crate) fn press(&mut self, point: (f64, f64)) {
        self.grid = luxforge_core::path::PathCapture::default();
        self.first = None;
        self.last = None;
        self.captured = 0;
        self.painting = true;
        *self.points.get_mut() = None;
        self.push([point.0, point.1]);
    }

    /// The pointer moved with the button down. A position identical to the last one is dropped here
    /// rather than posted, and a failed capture takes nothing more: it stays failed until the next
    /// press. Answers whether the path took the position.
    pub(crate) fn paint(&mut self, point: (f64, f64)) -> bool {
        let point = [point.0, point.1];
        if !self.painting || self.grid.capture_error().is_some() || self.last == Some(point) {
            return false;
        }
        self.push(point);
        true
    }

    fn push(&mut self, point: [f64; 2]) {
        let held = self.grid.held();
        self.grid.push(point);
        self.first.get_or_insert(point);
        self.last = Some(point);
        self.captured = self.captured.saturating_add(1);
        // A position that snapped into the cell before it is not held and changes no posted path;
        // a new cell or a refusal does.
        if self.grid.held() != held || self.grid.capture_error().is_some() {
            *self.points.get_mut() = None;
        }
    }

    /// The pointer came up. The path it drew stays; committing it is a separate decision.
    pub(crate) fn release(&mut self) {
        self.painting = false;
    }

    /// How many positions the pointer painted, for bounded evidence and diagnostics.
    pub(crate) fn captured(&self) -> usize {
        self.captured
    }

    pub(crate) fn painting(&self) -> bool {
        self.painting
    }

    /// Where the path began: a correction's pin and a straight stroke's start.
    pub(crate) fn first(&self) -> Option<[f64; 2]> {
        self.first
    }

    /// Where the path has got to: the next straight stroke's start.
    pub(crate) fn last(&self) -> Option<[f64; 2]> {
        self.last
    }

    /// The path this capture posts at `radius`: the host's own stored stroke of the captured path
    /// ([`luxforge_core::path::PathCapture::stroke`]). Deterministic, so the same captured path at
    /// the same radius is always the same stored stroke and therefore the same content address.
    /// Shared with the cache, so reading it copies nothing.
    pub(crate) fn posted(&self, radius: f64) -> Result<Arc<[[f64; 2]]>, luxforge_core::Error> {
        self.prepare(radius)?;
        Ok(self
            .points
            .borrow()
            .as_ref()
            .expect("the reduction was cached")
            .points
            .clone())
    }

    /// Reduce only after a change of the held path or the radius.
    fn prepare(&self, radius: f64) -> Result<(), luxforge_core::Error> {
        let mut cached = self.points.borrow_mut();
        if cached
            .as_ref()
            .is_none_or(|points| points.radius != radius.to_bits())
        {
            #[cfg(test)]
            counts::decimated();
            *cached = Some(CachedPoints {
                radius: radius.to_bits(),
                points: self.grid.stroke(radius)?.into(),
            });
        }
        Ok(())
    }

    /// A drawn path's actual capture/decimation failure at `radius`, before any draft request or
    /// commit.
    pub(crate) fn capture_error(&self, radius: f64) -> Option<luxforge_core::Error> {
        self.drawn().then(|| self.prepare(radius).err()).flatten()
    }

    /// The count a readout reports without processing the path again. Unknown until first posted.
    pub(crate) fn posted_count(&self) -> Option<usize> {
        self.points
            .borrow()
            .as_ref()
            .map(|points| points.points.len())
    }

    /// Something was drawn. An empty path commits nothing: a press that painted no position is not
    /// an edit, and a commit of one would be a history entry nobody made.
    pub(crate) fn drawn(&self) -> bool {
        self.captured > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RADIUS: f64 = 0.1;

    #[test]
    fn live_capture_is_bounded_by_the_grid_and_failure_is_sticky_until_a_new_press() {
        use luxforge_core::path::{CAPTURED_POINTS_PER_STROKE, COORDINATE_STEPS_PER_UNIT};
        let mut stroke = StrokeCapture::new();
        stroke.press((0.0, 0.3));
        // Positions inside one grid cell store nothing and consume no bound.
        for index in 1..1000 {
            assert!(stroke.paint((index as f64 * 1e-9, 0.3)));
        }
        assert!(stroke.capture_error(RADIUS).is_none());
        for index in 1..CAPTURED_POINTS_PER_STROKE {
            assert!(stroke.paint((index as f64 / COORDINATE_STEPS_PER_UNIT, 0.3)));
        }
        assert_eq!(stroke.posted(RADIUS).unwrap().len(), 2);
        assert!(stroke.paint((0.5, 0.5)));
        let error = stroke.posted(RADIUS).unwrap_err();
        assert_eq!(error.kind, luxforge_core::ErrorKind::ResourceLimit);
        assert!(error.detail.contains("16384 captured positions"));
        let captured = stroke.captured();
        for _ in 0..100 {
            assert!(!stroke.paint((0.6, 0.6)));
        }
        assert_eq!(
            stroke.captured(),
            captured,
            "a failed capture takes nothing more"
        );
        assert_eq!(stroke.capture_error(RADIUS).unwrap().detail, error.detail);
        stroke.press((0.4, 0.5));
        assert!(stroke.capture_error(RADIUS).is_none());
        assert_eq!(
            &*stroke.posted(RADIUS).unwrap(),
            luxforge_core::path::decimate(&[[0.4, 0.5]], RADIUS).unwrap()
        );
    }

    #[test]
    fn invalid_capture_never_becomes_an_empty_or_partial_posted_path() {
        for point in [
            (2.1, 0.5),
            (0.5, -1.1),
            (f64::NAN, 0.5),
            (0.5, f64::INFINITY),
        ] {
            let mut stroke = StrokeCapture::new();
            stroke.press((0.2, 0.3));
            assert!(stroke.paint(point));
            let error = stroke.capture_error(RADIUS).unwrap();
            assert_eq!(error.kind, luxforge_core::ErrorKind::Validation);
            assert!(error.detail.contains("path position 1"));
            assert_eq!(stroke.posted(RADIUS).unwrap_err().detail, error.detail);
            assert!(!stroke.paint((0.4, 0.4)));
        }
    }

    #[test]
    fn posted_path_is_the_core_path_and_reduces_only_once_until_input_or_radius_changes() {
        let mut stroke = StrokeCapture::new();
        let mut path = vec![[0.2, 0.3]];
        stroke.press((0.2, 0.3));
        for index in 1..200 {
            let point = (
                0.2 + index as f64 * 0.002,
                0.3 + (index as f64 * 0.1).sin() * 0.04,
            );
            stroke.paint(point);
            path.push([point.0, point.1]);
        }
        assert_eq!(stroke.posted_count(), None);
        let untouched = stroke.clone();
        let expected = luxforge_core::path::decimate(&path, RADIUS).unwrap();
        assert_eq!(&*stroke.posted(RADIUS).unwrap(), expected);
        let allocation = stroke.points.borrow().as_ref().unwrap().points.as_ptr();
        assert_eq!(&*stroke.posted(RADIUS).unwrap(), expected);
        assert!(stroke.capture_error(RADIUS).is_none());
        assert_eq!(stroke.posted_count(), Some(expected.len()));
        assert_eq!(
            allocation,
            stroke.points.borrow().as_ref().unwrap().points.as_ptr()
        );
        assert_eq!(stroke, untouched, "memoizing does not change editing state");
        stroke.paint((0.8, 0.4));
        path.push([0.8, 0.4]);
        assert_eq!(stroke.posted_count(), None);
        assert_eq!(
            &*stroke.posted(RADIUS).unwrap(),
            luxforge_core::path::decimate(&path, RADIUS).unwrap()
        );
        assert_eq!(
            &*stroke.posted(0.01).unwrap(),
            luxforge_core::path::decimate(&path, 0.01).unwrap()
        );
        assert_eq!(stroke.first(), Some([0.2, 0.3]));
        assert_eq!(stroke.last(), Some([0.8, 0.4]));
    }

    #[test]
    fn a_finished_path_of_given_positions_is_the_same_capture_a_pointer_makes() {
        let mut drawn = StrokeCapture::new();
        drawn.press((0.25, 0.5));
        drawn.paint((0.75, 0.5));
        drawn.release();
        assert_eq!(StrokeCapture::of(&[[0.25, 0.5], [0.75, 0.5]]), drawn);
        let empty = StrokeCapture::of(&[]);
        assert!(!empty.drawn() && empty.capture_error(RADIUS).is_none());
    }
}
