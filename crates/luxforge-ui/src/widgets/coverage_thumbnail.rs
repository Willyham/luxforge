//! A mask's coverage thumbnail: a small greyscale grid, [`theme::THUMBNAIL_WIDTH`] ×
//! [`theme::THUMBNAIL_HEIGHT`] points on its near-black ground, rounded and outlined as the
//! mask-panels board draws `.th`.
//!
//! The cells are the coverage grid the exact preview already returns for a mask, reduced by the
//! caller. The thumbnail is a canvas of rectangles, not an image: an image handle made from pixels
//! in `view()` uploads a new texture every frame, while a canvas's [`canvas::Cache`] keeps the
//! tessellated rectangles until the caller's grid changes. Equal neighbours in a row are merged
//! into one rectangle, so a flat mask is a handful of rectangles, and a grid finer than the
//! thumbnail's points is sampled down to one cell per point first, so the geometry is bounded by
//! the thumbnail's size whatever the caller passes. The grid is shared as an `Arc<[u8]>`, so
//! building the model each frame copies no cells.

use super::curve_editor::invalidate_on_version_change;
use crate::theme;
use iced::widget::canvas;
use iced::{Color, Element, Length, Point, Rectangle, Renderer, Size, Theme};
use std::cell::Cell;
use std::sync::Arc;

/// Plain data for one coverage thumbnail.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CoverageThumbnailModel {
    /// Row-major coverage, one byte per cell: 0 is black (not selected), 255 white (selected).
    /// `None`, or a slice whose length is not `width × height`, draws the empty ground: the
    /// coverage is not known yet.
    pub cells: Option<Arc<[u8]>>,
    pub width: usize,
    pub height: usize,
    /// The grid's identity, from the caller. The drawing is rebuilt only when this, the grid's
    /// allocation or its size changes, so a caller that shares one `Arc` per grid and bumps this
    /// when the preview returns a new grid pays for one tessellation per new grid.
    pub version: u64,
}

/// The most cells the thumbnail draws across and down: one per point.
pub const THUMBNAIL_CELLS: (usize, usize) = (
    theme::THUMBNAIL_WIDTH as usize,
    theme::THUMBNAIL_HEIGHT as usize,
);

pub fn coverage_thumbnail<'a, M: 'a>(model: &CoverageThumbnailModel) -> Element<'a, M> {
    canvas(Thumbnail {
        model: model.clone(),
    })
    .width(Length::Fixed(theme::THUMBNAIL_WIDTH))
    .height(Length::Fixed(theme::THUMBNAIL_HEIGHT))
    .into()
}

/// The cells to draw and their grid size: the caller's grid, sampled down by nearest neighbour to
/// at most [`THUMBNAIL_CELLS`] when it is finer. `None` when the grid is missing, empty or its
/// length does not match its size.
pub fn thumbnail_grid(
    cells: Option<&[u8]>,
    width: usize,
    height: usize,
) -> Option<(Vec<u8>, usize, usize)> {
    let cells = cells?;
    if width == 0 || height == 0 || width.checked_mul(height)? != cells.len() {
        return None;
    }
    let (out_width, out_height) = (width.min(THUMBNAIL_CELLS.0), height.min(THUMBNAIL_CELLS.1));
    if (out_width, out_height) == (width, height) {
        return Some((cells.to_vec(), width, height));
    }
    // Each output cell takes the source cell under its centre.
    let pick = |index: usize, out: usize, source: usize| (index * 2 + 1) * source / (out * 2);
    let mut reduced = Vec::with_capacity(out_width * out_height);
    for y in 0..out_height {
        let row = pick(y, out_height, height) * width;
        for x in 0..out_width {
            reduced.push(cells[row + pick(x, out_width, width)]);
        }
    }
    Some((reduced, out_width, out_height))
}

/// One row's runs of equal cells, as `(first cell, length, value)`, left to right.
pub fn row_runs(row: &[u8]) -> Vec<(usize, usize, u8)> {
    let mut runs: Vec<(usize, usize, u8)> = Vec::new();
    for (index, &value) in row.iter().enumerate() {
        match runs.last_mut() {
            Some((_, length, last)) if *last == value => *length += 1,
            _ => runs.push((index, 1, value)),
        }
    }
    runs
}

struct Thumbnail {
    model: CoverageThumbnailModel,
}

/// What the cached drawing was built from: the caller's version, the grid's allocation and size.
type Key = (u64, usize, usize, usize);

#[derive(Default)]
struct ThumbnailState {
    cache: canvas::Cache,
    key: Cell<Option<Key>>,
}

impl<M> canvas::Program<M> for Thumbnail {
    type State = ThumbnailState;

    fn draw(
        &self,
        state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        if bounds.width <= 0.0 || bounds.height <= 0.0 {
            return Vec::new();
        }
        let model = &self.model;
        let address = model
            .cells
            .as_ref()
            .map_or(0, |cells| cells.as_ptr() as usize);
        invalidate_on_version_change(
            &state.key,
            (model.version, address, model.width, model.height),
            || state.cache.clear(),
        );
        vec![state.cache.draw(renderer, bounds.size(), |frame| {
            let size = frame.size();
            frame.fill(
                &canvas::Path::rounded_rectangle(
                    Point::ORIGIN,
                    size,
                    theme::THUMBNAIL_RADIUS.into(),
                ),
                theme::THUMBNAIL_BACKGROUND,
            );
            if let Some((cells, width, height)) =
                thumbnail_grid(model.cells.as_deref(), model.width, model.height)
            {
                let inset = theme::BORDER_WIDTH;
                let cell = Size::new(
                    (size.width - 2.0 * inset) / width as f32,
                    (size.height - 2.0 * inset) / height as f32,
                );
                for (y, row) in cells.chunks_exact(width).enumerate() {
                    for (x, length, value) in row_runs(row) {
                        if value == 0 {
                            continue; // the ground is already black
                        }
                        frame.fill_rectangle(
                            Point::new(
                                inset + x as f32 * cell.width,
                                inset + y as f32 * cell.height,
                            ),
                            Size::new(cell.width * length as f32, cell.height),
                            Color::from_rgb8(value, value, value),
                        );
                    }
                }
            }
            // The border goes on last, so it also rounds off the corner cells.
            let half = theme::BORDER_WIDTH / 2.0;
            frame.stroke(
                &canvas::Path::rounded_rectangle(
                    Point::new(half, half),
                    Size::new(size.width - 2.0 * half, size.height - 2.0 * half),
                    (theme::THUMBNAIL_RADIUS - half).into(),
                ),
                canvas::Stroke::default()
                    .with_color(theme::THUMBNAIL_BORDER)
                    .with_width(theme::BORDER_WIDTH),
            );
        })]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_grid_at_or_under_the_thumbnail_size_is_drawn_as_given() {
        let cells: Vec<u8> = (0..28 * 19).map(|i| i as u8).collect();
        assert_eq!(
            thumbnail_grid(Some(&cells), 28, 19),
            Some((cells.clone(), 28, 19))
        );
        assert_eq!(
            thumbnail_grid(Some(&[0, 255, 128, 64]), 2, 2),
            Some((vec![0, 255, 128, 64], 2, 2))
        );
    }

    #[test]
    fn a_finer_grid_is_sampled_down_to_one_cell_per_point() {
        // 56 × 38: each output cell takes the source cell under its centre, the odd ones.
        let cells: Vec<u8> = (0..56 * 38)
            .map(|i| if (i % 56) % 2 == 1 { 255 } else { 0 })
            .collect();
        let (reduced, width, height) = thumbnail_grid(Some(&cells), 56, 38).unwrap();
        assert_eq!((width, height), THUMBNAIL_CELLS);
        assert!(reduced.iter().all(|&value| value == 255));
        // Only the long side is reduced when only it is too fine.
        let wide = vec![7; 100 * 4];
        let (reduced, width, height) = thumbnail_grid(Some(&wide), 100, 4).unwrap();
        assert_eq!((width, height, reduced.len()), (28, 4, 28 * 4));
    }

    #[test]
    fn a_missing_or_mis_sized_grid_draws_the_placeholder() {
        assert_eq!(thumbnail_grid(None, 28, 19), None);
        assert_eq!(thumbnail_grid(Some(&[1, 2, 3]), 2, 2), None);
        assert_eq!(thumbnail_grid(Some(&[]), 0, 0), None);
        assert_eq!(thumbnail_grid(Some(&[1]), usize::MAX, 2), None);
    }

    #[test]
    fn equal_neighbours_merge_into_runs() {
        assert_eq!(row_runs(&[]), Vec::new());
        assert_eq!(row_runs(&[9; 5]), vec![(0, 5, 9)]);
        assert_eq!(
            row_runs(&[0, 0, 255, 255, 255, 0, 12]),
            vec![(0, 2, 0), (2, 3, 255), (5, 1, 0), (6, 1, 12)]
        );
    }

    #[test]
    fn every_state_builds() {
        let cells: Arc<[u8]> = (0..28 * 19).map(|i| (i % 256) as u8).collect();
        for model in [
            CoverageThumbnailModel::default(),
            CoverageThumbnailModel {
                cells: Some(cells),
                width: 28,
                height: 19,
                version: 1,
            },
        ] {
            let _: Element<'_, ()> = coverage_thumbnail(&model);
        }
    }
}
