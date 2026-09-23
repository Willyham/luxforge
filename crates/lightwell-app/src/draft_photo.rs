//! The crop draft's input stage on the GPU, in tiles the toolkit's image atlas holds whole.
//!
//! The toolkit stores an image larger than one atlas layer — 2048 px a side in `iced_wgpu` 0.14 —
//! as fragments, and its image shader places each fragment's corners by rotating them about the
//! image's centre but rotates the fragment's texture coordinates about the fragment's own centre.
//! A straightened draft of any photograph-sized input stage was therefore drawn as a grid of pieces
//! each turned in place, with the canvas showing between them: every RAW the editor opens is wider
//! than one layer. Here the stage is cut into tiles of at most one layer, so each is one
//! allocation that the shader rotates correctly about its own centre, and each is drawn with that
//! centre first rotated about the stage's centre. Together that is one rigid rotation of the whole
//! stage, which is the display filter the crop contract asks the draft for.
//!
//! Memory: the tiles hold the stage's pixels once, in place of the one buffer a single image held;
//! a stage within one layer is not copied at all. Cutting a larger one is a copy of the stage made
//! off the update thread, after which the render's own buffer is released.
use iced::{Rectangle, Size, widget::image};
use iced_runtime::image as image_memory;
use lightwell_core::Raster;
use std::sync::Arc;

/// The side of one `iced_wgpu` atlas layer, its `atlas::MAX_SIZE`. An image no larger than this
/// in either axis is stored as one allocation; anything larger is fragmented.
pub(crate) const TILE: u32 = 2048;

/// One tile's rectangle in stage pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TileRect {
    pub(crate) x: u32,
    pub(crate) y: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

/// The tiles a `width` × `height` stage is cut into, row by row: at most [`TILE`] a side, meeting
/// edge to edge and covering the stage exactly once.
pub(crate) fn tile_rects(width: u32, height: u32) -> Vec<TileRect> {
    let mut rects = Vec::new();
    for y in (0..height).step_by(TILE as usize) {
        for x in (0..width).step_by(TILE as usize) {
            rects.push(TileRect {
                x,
                y,
                width: TILE.min(width - x),
                height: TILE.min(height - y),
            });
        }
    }
    rects
}

/// One tile's own RGBA pixels, cut out of the stage.
pub(crate) struct Piece {
    pub(crate) rect: TileRect,
    pub(crate) pixels: Vec<u8>,
}

impl std::fmt::Debug for Piece {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Piece")
            .field("rect", &self.rect)
            .field("bytes", &self.pixels.len())
            .finish()
    }
}

/// Copy every tile of a stage larger than one layer out of its RGBA buffer, row by row.
pub(crate) fn cut(raster: &Raster) -> Vec<Piece> {
    let stride = raster.width as usize * 4;
    tile_rects(raster.width, raster.height)
        .into_iter()
        .map(|rect| {
            let row = rect.width as usize * 4;
            let mut pixels = Vec::with_capacity(row * rect.height as usize);
            for y in rect.y..rect.y + rect.height {
                let start = y as usize * stride + rect.x as usize * 4;
                pixels.extend_from_slice(&raster.rgba[start..start + row]);
            }
            Piece { rect, pixels }
        })
        .collect()
}

/// The handles to upload for a stage: the raster's own buffer when it fits one layer, otherwise
/// the pieces [`cut`] made.
pub(crate) fn handles(pieces: Vec<Piece>) -> Vec<(TileRect, image::Handle)> {
    pieces
        .into_iter()
        .map(|piece| {
            let handle = image::Handle::from_rgba(
                piece.rect.width,
                piece.rect.height,
                iced_runtime::core::Bytes::from(piece.pixels),
            );
            (piece.rect, handle)
        })
        .collect()
}

/// The whole stage as one handle, for a stage within one layer: the render's own buffer, uncopied.
pub(crate) fn whole(raster: Raster) -> (TileRect, image::Handle) {
    let rect = TileRect {
        x: 0,
        y: 0,
        width: raster.width,
        height: raster.height,
    };
    let handle = image::Handle::from_rgba(
        raster.width,
        raster.height,
        iced_runtime::core::Bytes::from_owner(raster.rgba),
    );
    (rect, handle)
}

/// Whether a stage of this size must be cut before it is uploaded.
pub(crate) fn needs_cutting(width: u32, height: u32) -> bool {
    width > TILE || height > TILE
}

/// The crop layer's input stage, every tile of it on the GPU.
#[derive(Clone, Debug)]
pub(crate) struct DraftPhoto {
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// What the canvas draws: each tile's rectangle and handle.
    pub(crate) tiles: Arc<Vec<(TileRect, image::Handle)>>,
    /// What keeps every tile on the GPU for as long as the draft shows it.
    allocations: Arc<Vec<image_memory::Allocation>>,
}

impl DraftPhoto {
    /// A stage whose tiles are drawn from their handles with nothing holding them on the GPU, for
    /// tests of the drawing geometry, which runs no renderer.
    #[cfg(test)]
    pub(crate) fn unallocated(width: u32, height: u32) -> Self {
        let tiles = tile_rects(width, height)
            .into_iter()
            .map(|rect| {
                let pixels = vec![0u8; rect.width as usize * rect.height as usize * 4];
                (
                    rect,
                    image::Handle::from_rgba(rect.width, rect.height, pixels),
                )
            })
            .collect();
        Self {
            width,
            height,
            tiles: Arc::new(tiles),
            allocations: Arc::new(Vec::new()),
        }
    }

    /// How many tiles hold the stage on the GPU.
    pub(crate) fn allocated(&self) -> usize {
        self.allocations.len()
    }
}

/// A stage being uploaded: its size, its tiles, and each tile's allocation once it has arrived.
#[derive(Debug)]
pub(crate) struct Assembly {
    pub(crate) generation: u64,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) tiles: Vec<(TileRect, Option<image_memory::Allocation>)>,
}

impl Assembly {
    /// Record one tile's allocation, and hand back the whole photo once the last one is in.
    pub(crate) fn arrived(
        &mut self,
        index: usize,
        allocation: image_memory::Allocation,
    ) -> Option<DraftPhoto> {
        if let Some((_, slot)) = self.tiles.get_mut(index) {
            *slot = Some(allocation);
        }
        if self.tiles.iter().any(|(_, slot)| slot.is_none()) {
            return None;
        }
        let (tiles, allocations) = self
            .tiles
            .iter()
            .map(|(rect, slot)| {
                let allocation = slot.clone().expect("every slot is filled");
                ((*rect, allocation.handle().clone()), allocation)
            })
            .unzip();
        Some(DraftPhoto {
            width: self.width,
            height: self.height,
            tiles: Arc::new(tiles),
            allocations: Arc::new(allocations),
        })
    }
}

/// Where one tile is drawn when the whole stage, laid out unrotated in `stage_bounds`, turns by
/// `angle` radians about its centre: the tile's own unrotated rectangle moved so that its centre is
/// where the stage's rotation takes that centre. Drawn there with the same `angle` about its own
/// centre, the tile lands exactly where it lies in the rotated stage.
///
/// The rotation is the toolkit's image rotation, which is the crop contract's matrix: in y-down
/// canvas pixels a positive angle turns clockwise, `(x, y) ↦ (x cos θ − y sin θ, x sin θ + y cos θ)`
/// about the centre.
pub(crate) fn placed(
    stage_bounds: Rectangle,
    stage: (u32, u32),
    tile: TileRect,
    angle: f32,
) -> Rectangle {
    let scale_x = stage_bounds.width / stage.0 as f32;
    let scale_y = stage_bounds.height / stage.1 as f32;
    let size = Size::new(tile.width as f32 * scale_x, tile.height as f32 * scale_y);
    let centre = stage_bounds.center();
    let dx = stage_bounds.x + (tile.x as f32 + tile.width as f32 / 2.0) * scale_x - centre.x;
    let dy = stage_bounds.y + (tile.y as f32 + tile.height as f32 / 2.0) * scale_y - centre.y;
    let (sin, cos) = angle.sin_cos();
    let moved = (
        centre.x + dx * cos - dy * sin,
        centre.y + dx * sin + dy * cos,
    );
    Rectangle::new(
        iced::Point::new(moved.0 - size.width / 2.0, moved.1 - size.height / 2.0),
        size,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightwell_core::SnapshotId;

    fn raster(width: u32, height: u32) -> Raster {
        let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
        for y in 0..height {
            for x in 0..width {
                rgba.extend_from_slice(&[
                    (x % 251) as u8,
                    (y % 241) as u8,
                    ((x + y) % 239) as u8,
                    255,
                ]);
            }
        }
        Raster {
            width,
            height,
            rgba: rgba.into(),
            source_fingerprint: "f".into(),
            snapshot_id: SnapshotId::new(),
        }
    }

    /// Every photograph-sized stage is cut into layer-sized tiles that cover it exactly once; a
    /// stage within one layer is not cut at all.
    #[test]
    fn tiles_cover_the_stage_once_and_fit_one_layer() {
        for (width, height) in [
            (7728, 5152),
            (4024, 6048),
            (5464, 3640),
            (2048, 2048),
            (2049, 17),
        ] {
            let rects = tile_rects(width, height);
            let mut covered = vec![0u8; width as usize * height as usize];
            for rect in &rects {
                assert!(rect.width <= TILE && rect.height <= TILE && rect.width > 0);
                for y in rect.y..rect.y + rect.height {
                    for x in rect.x..rect.x + rect.width {
                        covered[y as usize * width as usize + x as usize] += 1;
                    }
                }
            }
            assert!(covered.iter().all(|count| *count == 1), "{width}x{height}");
            assert_eq!(needs_cutting(width, height), rects.len() > 1);
        }
        assert_eq!(tile_rects(7728, 5152).len(), 12);
        assert!(!needs_cutting(480, 320));
    }

    /// A piece holds exactly the stage pixels of its rectangle.
    #[test]
    fn a_piece_is_its_rectangle_of_the_stage() {
        let stage = raster(4100, 2100);
        let pieces = cut(&stage);
        assert_eq!(pieces.len(), 6);
        for piece in &pieces {
            let rect = piece.rect;
            assert_eq!(
                piece.pixels.len(),
                rect.width as usize * rect.height as usize * 4
            );
            for (y, x) in [
                (0, 0),
                (rect.height - 1, rect.width - 1),
                (rect.height / 2, rect.width / 3),
            ] {
                let at = (y as usize * rect.width as usize + x as usize) * 4;
                assert_eq!(
                    stage.pixel(rect.x + x, rect.y + y).unwrap(),
                    [
                        piece.pixels[at],
                        piece.pixels[at + 1],
                        piece.pixels[at + 2],
                        piece.pixels[at + 3]
                    ]
                );
            }
        }
    }

    /// Placed tiles are one rigid rotation of the stage: every tile's centre is the stage rotation
    /// of where it lay, adjacent tiles still share their edge once each is turned about its own
    /// centre, and at 0° every tile is exactly its own part of the stage.
    #[test]
    fn placed_tiles_rotate_as_one_stage() {
        let stage = (7728_u32, 5152_u32);
        let bounds = Rectangle::new(iced::Point::new(40.0, 30.0), Size::new(1545.6, 1030.4));
        let rects = tile_rects(stage.0, stage.1);
        for rect in &rects {
            let at_zero = placed(bounds, stage, *rect, 0.0);
            let scale = bounds.width / stage.0 as f32;
            assert!((at_zero.x - (bounds.x + rect.x as f32 * scale)).abs() < 1e-3);
            assert!((at_zero.y - (bounds.y + rect.y as f32 * scale)).abs() < 1e-3);
        }
        // Rotate a point of the stage the way the toolkit rotates an image about its bounds'
        // centre, so the check does not reuse `placed`'s own arithmetic.
        let turn = |point: (f32, f32), about: (f32, f32), angle: f32| {
            let (sin, cos) = angle.sin_cos();
            let (dx, dy) = (point.0 - about.0, point.1 - about.1);
            (about.0 + dx * cos - dy * sin, about.1 + dx * sin + dy * cos)
        };
        for degrees in [7.0_f32, -12.5, 44.0] {
            let angle = degrees.to_radians();
            let centre = (bounds.center().x, bounds.center().y);
            let scale = bounds.width / stage.0 as f32;
            for rect in &rects {
                let tile = placed(bounds, stage, *rect, angle);
                let tile_centre = (tile.center().x, tile.center().y);
                // The tile's top-left corner, as the whole stage's rotation moves it and as the
                // tile's own rotation about its placed centre moves it: the same point.
                let corner = (
                    bounds.x + rect.x as f32 * scale,
                    bounds.y + rect.y as f32 * scale,
                );
                let by_stage = turn(corner, centre, angle);
                let by_tile = turn((tile.x, tile.y), tile_centre, angle);
                assert!(
                    (by_stage.0 - by_tile.0).abs() < 1e-2 && (by_stage.1 - by_tile.1).abs() < 1e-2,
                    "{degrees}°: tile {rect:?} corner {by_tile:?}, stage says {by_stage:?}"
                );
            }
        }
    }
}
