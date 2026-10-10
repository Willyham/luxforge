//! The frozen-patch composite, the CPU reference (`docs/design/corrections.md`, "Frozen patches").
//!
//! A patch is `width × height` RGBA IEEE half floats, little-endian (`rgba16f-le`): RGB in the
//! repair stage's representation — linear sRGB/D65, which both the decoded JPEG and the developed
//! RAW the repair stage receives are in — and A the coverage `0..=1`, not premultiplied. It is
//! placed 1:1 at `rect` in the content stage it was generated for.
//!
//! - **Full resolution** (the stage the rectangle is in): every pixel `p` of `rect` with coverage
//!   `c = α · opacity / 100 > 0` becomes `in + c · (patch − in)`, computed in `f64` and rounded to
//!   `f32` as the frame holds it. A pixel with `c = 0`, and every pixel outside `rect`, is never
//!   written, so it is exactly its input.
//! - **A reduced stage** (a GPU fit frame drawn at a fraction of the content size): each stage
//!   pixel covers a box of content pixels; the patch's coverage `Ā` and coverage-weighted colour
//!   `P̄` are the box's area-weighted means (pixels outside `rect` count as no coverage), and the
//!   pixel becomes `in + o · (P̄ − Ā · in)` with `o = opacity / 100` — the full-resolution composite
//!   averaged over the box with the input taken constant across it. The means are built once per
//!   bound stage, on the render worker that first needs them, `O(patch pixels)`.
//!
//! A coverage that is not finite or not above zero is no coverage, and one above one is one; a
//! colour that is not finite leaves its pixel uncovered. The producer's publication refused such
//! values already; the composite still never writes one.
use super::RepairFrame;
use crate::{
    Cancel, Error,
    artifacts::PreparedArtifact,
    modules::{Region, Stage},
};
use std::sync::Arc;

/// The bytes one stored patch pixel takes: four half floats.
pub(crate) const PATCH_PIXEL_BYTES: u64 = 8;

/// A patch as a repair table row carries it: its verified bytes, its rectangle in content pixels,
/// and the content stage that rectangle is in.
#[derive(Clone, Debug)]
pub(crate) struct PatchSource {
    pub(crate) artifact: Arc<PreparedArtifact>,
    pub(crate) rect: [u32; 4],
    pub(crate) full: Stage,
}

impl PartialEq for PatchSource {
    /// The same artifact at the same place in the same stage: the bytes are content-addressed.
    fn eq(&self, other: &Self) -> bool {
        self.artifact.id == other.artifact.id && self.rect == other.rect && self.full == other.full
    }
}

impl Eq for PatchSource {}

impl PatchSource {
    /// The rectangle as a region of the content stage, clipped to it: a stale patch whose stage
    /// shrank is clipped.
    fn content_region(&self) -> Region {
        let [x, y, width, height] = self.rect;
        let x1 = x.saturating_add(width).min(self.full.width);
        let y1 = y.saturating_add(height).min(self.full.height);
        Region {
            x0: x.min(x1),
            y0: y.min(y1),
            width: x1.saturating_sub(x),
            height: y1.saturating_sub(y),
        }
    }

    /// The stage pixels the patch may write at `stage`: its clipped rectangle, scaled out to whole
    /// pixels at a reduced stage. `O(1)`.
    pub(crate) fn writes(&self, stage: Stage) -> Region {
        let content = self.content_region();
        if content.is_empty() {
            return Region::EMPTY;
        }
        if stage == self.full {
            return content;
        }
        let (sx, sy) = scales(stage, self.full);
        let x0 = (f64::from(content.x0) * sx).floor() as u32;
        let y0 = (f64::from(content.y0) * sy).floor() as u32;
        let x1 = ((f64::from(content.x1()) * sx).ceil() as u32).min(stage.width);
        let y1 = ((f64::from(content.y1()) * sy).ceil() as u32).min(stage.height);
        if x0 >= x1 || y0 >= y1 {
            return Region::EMPTY;
        }
        Region {
            x0,
            y0,
            width: x1 - x0,
            height: y1 - y0,
        }
    }

    /// The pixel `(x, y)` of the patch, `(x, y)` within its own `width × height`.
    #[inline]
    fn texel(&self, x: u32, y: u32) -> [f32; 4] {
        let width = self.rect[2] as usize;
        let at = (y as usize * width + x as usize) * PATCH_PIXEL_BYTES as usize;
        let bytes = &self.artifact.bytes[at..at + PATCH_PIXEL_BYTES as usize];
        [0, 1, 2, 3].map(|channel| {
            half::f16::from_le_bytes([bytes[2 * channel], bytes[2 * channel + 1]]).to_f32()
        })
    }

    /// Check the bytes hold exactly the rectangle's pixels.
    pub(crate) fn check(&self, name: &str) -> Result<(), Error> {
        let [_, _, width, height] = self.rect;
        let expected = u64::from(width) * u64::from(height) * PATCH_PIXEL_BYTES;
        if width == 0 || height == 0 || self.artifact.bytes.len() as u64 != expected {
            return Err(Error::source_unavailable(format!(
                "artifact {} of {name} does not hold its {width}x{height} patch",
                self.artifact.id
            )));
        }
        if self.full.width == 0 || self.full.height == 0 {
            return Err(Error::validation(format!(
                "{name} is placed in an empty stage"
            )));
        }
        Ok(())
    }
}

fn scales(stage: Stage, full: Stage) -> (f64, f64) {
    (
        f64::from(stage.width) / f64::from(full.width),
        f64::from(stage.height) / f64::from(full.height),
    )
}

/// A texel's straight colour and coverage, or `None` when it covers nothing.
#[inline]
fn covering(texel: [f32; 4]) -> Option<([f64; 3], f64)> {
    let alpha = texel[3];
    if !alpha.is_finite() || alpha <= 0.0 || !texel[..3].iter().all(|value| value.is_finite()) {
        return None;
    }
    Some((
        [texel[0], texel[1], texel[2]].map(f64::from),
        f64::from(alpha.min(1.0)),
    ))
}

/// A patch bound to the stage a repair frame is drawn at.
#[derive(Debug)]
pub(crate) struct BoundPatch {
    source: PatchSource,
    stage: Stage,
    /// `opacity / 100`.
    amount: f64,
    region: Region,
    /// At a reduced stage, per pixel of `region` row by row: the box's coverage-weighted colour
    /// `P̄` and its coverage `Ā`, shared through [`drawn`]'s cache. `None` at full resolution,
    /// which reads the bytes directly.
    reduced: Option<Arc<Drawn>>,
}

impl BoundPatch {
    /// `source` at `stage` with `opacity` in `0..=100`.
    pub(crate) fn new(source: PatchSource, opacity: u8, stage: Stage) -> Self {
        let region = source.writes(stage);
        let reduced = (stage != source.full && !region.is_empty()).then(|| drawn(&source, stage));
        Self {
            source,
            stage,
            amount: f64::from(opacity.min(100)) / 100.0,
            region,
            reduced,
        }
    }

    /// The pixels it may write.
    pub(crate) fn region(&self) -> Region {
        self.region
    }

    pub(crate) fn stage(&self) -> Stage {
        self.stage
    }
}

/// What a patch is drawn from at one stage, shared by the CPU reference and the GPU: at full
/// resolution its bytes as words, two a pixel (`rgba16f-le` read as `u32`); at a reduced stage the
/// box means of `region`, as `f64` for the reference and four `f32` words a pixel for the GPU.
#[derive(Debug)]
pub(crate) struct Drawn {
    pub(crate) region: Region,
    pub(crate) means: Vec<[f64; 4]>,
    pub(crate) words: Arc<[u32]>,
    /// The words are reduced means rather than the patch's own half floats.
    pub(crate) reduced: bool,
}

impl Drawn {
    fn bytes(&self) -> u64 {
        self.means.len() as u64 * 32 + self.words.len() as u64 * 4
    }
}

/// The most bytes the drawn-patch cache keeps: 128 MiB. Entries a plan or frame still holds stay
/// alive with it; the cache drops the least recently used beyond the bound.
const DRAWN_BYTES: u64 = 128 * 1024 * 1024;

type DrawnKey = (crate::ArtifactId, [u32; 4], Stage, Stage);

#[derive(Default)]
struct DrawnCache {
    entries: Vec<(DrawnKey, Arc<Drawn>)>,
    bytes: u64,
}

static DRAWN: std::sync::Mutex<DrawnCache> = std::sync::Mutex::new(DrawnCache {
    entries: Vec::new(),
    bytes: 0,
});

/// What `source` is drawn from at `stage` ([`Drawn`]), built once and kept in a bounded cache so a
/// gesture's ticks and the reference's frames share it: `O(patch pixels)` the first time, a lookup
/// after.
pub(crate) fn drawn(source: &PatchSource, stage: Stage) -> Arc<Drawn> {
    let key = (source.artifact.id.clone(), source.rect, source.full, stage);
    {
        let mut cache = DRAWN.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(at) = cache.entries.iter().position(|(held, _)| *held == key) {
            let entry = cache.entries.remove(at);
            let found = entry.1.clone();
            cache.entries.push(entry);
            return found;
        }
    }
    let built = Arc::new(if stage == source.full {
        Drawn {
            region: source.writes(stage),
            means: Vec::new(),
            words: source
                .artifact
                .bytes
                .chunks_exact(4)
                .map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
                .collect(),
            reduced: false,
        }
    } else {
        let region = source.writes(stage);
        let means = if region.is_empty() {
            Vec::new()
        } else {
            reduced_means(source, stage, region)
        };
        let words = means
            .iter()
            .flat_map(|mean| mean.map(|value| (value as f32).to_bits()))
            .collect();
        Drawn {
            region,
            means,
            words,
            reduced: true,
        }
    });
    let mut cache = DRAWN.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    cache.bytes += built.bytes();
    cache.entries.push((key, built.clone()));
    while cache.bytes > DRAWN_BYTES && cache.entries.len() > 1 {
        let (_, evicted) = cache.entries.remove(0);
        cache.bytes -= evicted.bytes();
    }
    built
}

/// One axis of a box filter: for each output index `o` of `0..count` starting at `start`, the
/// texel indices of `0..texels` (texel `t` at content `origin + t`) it overlaps and by how much,
/// in content pixels. `scale` is output pixels per content pixel.
fn axis(start: u32, count: u32, origin: u32, texels: u32, scale: f64) -> Vec<Vec<(u32, f64)>> {
    (start..start + count)
        .map(|output| {
            let low = f64::from(output) / scale;
            let high = f64::from(output + 1) / scale;
            let first = (low.floor().max(f64::from(origin))) as u32;
            let last = (high.ceil().min(f64::from(origin + texels))) as u32;
            (first..last.max(first))
                .filter_map(|content| {
                    let overlap = (f64::from(content + 1)).min(high) - f64::from(content).max(low);
                    (overlap > 0.0).then_some((content - origin, overlap))
                })
                .collect()
        })
        .collect()
}

/// The area-weighted means of every pixel of `region` at `stage` ([module](self) docs).
fn reduced_means(source: &PatchSource, stage: Stage, region: Region) -> Vec<[f64; 4]> {
    let content = source.content_region();
    let (sx, sy) = scales(stage, source.full);
    let (ox, oy) = (source.rect[0], source.rect[1]);
    // Texels past the clipped rectangle are not part of the content stage.
    let columns = axis(
        region.x0,
        region.width,
        ox,
        content.x1().saturating_sub(ox),
        sx,
    );
    let rows = axis(
        region.y0,
        region.height,
        oy,
        content.y1().saturating_sub(oy),
        sy,
    );
    let area = (1.0 / sx) * (1.0 / sy);
    let mut means = Vec::with_capacity(region.pixels() as usize);
    for row in &rows {
        for column in &columns {
            let mut sum = [0.0f64; 4];
            for &(ty, wy) in row {
                for &(tx, wx) in column {
                    if let Some((rgb, alpha)) = covering(source.texel(tx, ty)) {
                        let weight = wx * wy * alpha;
                        sum[0] += weight * rgb[0];
                        sum[1] += weight * rgb[1];
                        sum[2] += weight * rgb[2];
                        sum[3] += wx * wy * alpha;
                    }
                }
            }
            means.push(sum.map(|value| value / area));
        }
    }
    means
}

/// Composite `patch` over `frame` in place ([module](self) docs). The frame must hold the patch's
/// region in one part.
pub(crate) fn composite(
    frame: &mut RepairFrame<'_>,
    patch: &BoundPatch,
    cancel: &Cancel,
) -> Result<(), Error> {
    let region = patch.region;
    if region.is_empty() || patch.amount == 0.0 {
        return Ok(());
    }
    let means = patch.reduced.as_ref().map(|drawn| &drawn.means);
    for y in region.y0..region.y1() {
        cancel.check()?;
        for x in region.x0..region.x1() {
            let input = frame.get(x, y);
            let out = if let Some(means) = means {
                let at = ((y - region.y0) * region.width + (x - region.x0)) as usize;
                let [r, g, b, coverage] = means[at];
                if coverage <= 0.0 {
                    continue;
                }
                let o = patch.amount;
                let mean = [r, g, b];
                [0, 1, 2].map(|channel| {
                    let i = f64::from(input[channel]);
                    (i + o * (mean[channel] - coverage * i)) as f32
                })
            } else {
                let texel = patch.source.texel(x - patch.source.rect[0], y - patch.source.rect[1]);
                let Some((rgb, alpha)) = covering(texel) else {
                    continue;
                };
                let c = alpha * patch.amount;
                [0, 1, 2].map(|channel| {
                    let i = f64::from(input[channel]);
                    (i + c * (rgb[channel] - i)) as f32
                })
            };
            frame.set(x, y, out);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::{ArtifactId, ArtifactMeta};

    /// A patch of `width × height` from per-pixel RGBA.
    pub(crate) fn source(
        rect: [u32; 4],
        full: Stage,
        pixel: impl Fn(u32, u32) -> [f32; 4],
    ) -> PatchSource {
        let mut bytes = Vec::new();
        for y in 0..rect[3] {
            for x in 0..rect[2] {
                for value in pixel(x, y) {
                    bytes.extend_from_slice(&half::f16::from_f32(value).to_le_bytes());
                }
            }
        }
        let meta = ArtifactMeta {
            kind: "repair-patch".into(),
            width: Some(rect[2]),
            height: Some(rect[3]),
            colour: Some("linear-srgb".into()),
        };
        use sha2::Digest;
        let id = ArtifactId::for_hash(&format!("{:x}", sha2::Sha256::digest(&bytes))).unwrap();
        PatchSource {
            artifact: Arc::new(PreparedArtifact::new(id, &meta, bytes.into())),
            rect,
            full,
        }
    }

    fn stage(width: u32, height: u32) -> Stage {
        Stage { width, height }
    }

    fn frame_values(stage: Stage) -> Vec<f32> {
        let len = (stage.width * stage.height) as usize;
        (0..3 * len)
            .map(|index| (index as f32 * 0.37).sin() * 0.5 + 0.5)
            .collect()
    }

    #[test]
    fn outside_coverage_is_exactly_the_input_and_inside_is_the_frozen_blend() {
        let full = stage(12, 9);
        // Coverage 0 on the patch's left column, 0.5 elsewhere, colour (0.25, 0.5, 2.0).
        let patch = source([3, 2, 5, 4], full, |x, _| {
            [0.25, 0.5, 2.0, if x == 0 { 0.0 } else { 0.5 }]
        });
        let bound = BoundPatch::new(patch, 80, full);
        assert_eq!(
            bound.region(),
            Region {
                x0: 3,
                y0: 2,
                width: 5,
                height: 4
            }
        );
        let input = frame_values(full);
        let mut values = input.clone();
        let whole = Region::whole(full);
        let mut frame = RepairFrame::new(full, whole, &mut values).unwrap();
        composite(&mut frame, &bound, &Cancel::never()).unwrap();
        let len = (full.width * full.height) as usize;
        for y in 0..full.height {
            for x in 0..full.width {
                let at = (y * full.width + x) as usize;
                let inside = (4..8).contains(&x) && (2..6).contains(&y);
                for channel in 0..3 {
                    let before = input[channel * len + at];
                    let after = values[channel * len + at];
                    if inside {
                        let target = [0.25, 0.5, 2.0][channel];
                        let c = 0.5 * 0.8;
                        let expected = (f64::from(before) + c * (target - f64::from(before))) as f32;
                        assert_eq!(after.to_bits(), expected.to_bits(), "({x}, {y})");
                    } else {
                        assert_eq!(after.to_bits(), before.to_bits(), "({x}, {y}) untouched");
                    }
                }
            }
        }
    }

    #[test]
    fn a_reduced_stage_takes_the_box_means_of_the_full_composite() {
        let full = stage(8, 8);
        // A uniform patch of coverage 1 over [2, 6) x [2, 6): at half scale it covers whole
        // pixels [1, 3) x [1, 3) exactly, and the composite is the patch colour.
        let patch = source([2, 2, 4, 4], full, |_, _| [0.75, 0.25, 0.5, 1.0]);
        let half = stage(4, 4);
        let bound = BoundPatch::new(patch.clone(), 100, half);
        assert_eq!(
            bound.region(),
            Region {
                x0: 1,
                y0: 1,
                width: 2,
                height: 2
            }
        );
        let mut values = frame_values(half);
        let input = values.clone();
        let mut frame = RepairFrame::new(half, Region::whole(half), &mut values).unwrap();
        composite(&mut frame, &bound, &Cancel::never()).unwrap();
        let len = 16;
        for (x, y) in [(1u32, 1u32), (2, 2)] {
            let at = (y * 4 + x) as usize;
            for (channel, target) in [0.75f32, 0.25, 0.5].into_iter().enumerate() {
                assert!((values[channel * len + at] - target).abs() < 1e-6);
            }
        }
        assert_eq!(values[0], input[0], "outside the region");
        // An odd rectangle at a third: a pixel half covered blends half way.
        let third = stage(3, 3);
        let edge = source([0, 0, 2, 3], stage(6, 6), |_, _| [1.0, 1.0, 1.0, 1.0]);
        let bound = BoundPatch::new(edge, 100, third);
        assert_eq!(bound.region().width, 1);
        let mut values = vec![0.0f32; 27];
        let mut frame = RepairFrame::new(third, Region::whole(third), &mut values).unwrap();
        composite(&mut frame, &bound, &Cancel::never()).unwrap();
        assert!((values[0] - 1.0).abs() < 1e-9, "fully covered");
        assert_eq!(values[1], 0.0, "outside its region");
        assert!((values[3] - 0.5).abs() < 1e-9, "half covered blends half way");
    }

    #[test]
    fn non_finite_or_absent_coverage_never_writes() {
        let full = stage(4, 1);
        let patch = source([0, 0, 4, 1], full, |x, _| match x {
            0 => [1.0, 1.0, 1.0, f32::NAN],
            1 => [f32::INFINITY, 1.0, 1.0, 1.0],
            2 => [1.0, 1.0, 1.0, -0.5],
            _ => [1.0, 1.0, 1.0, 2.0],
        });
        let mut values = vec![0.5f32; 12];
        let mut frame = RepairFrame::new(full, Region::whole(full), &mut values).unwrap();
        composite(&mut frame, &BoundPatch::new(patch, 100, full), &Cancel::never()).unwrap();
        assert_eq!(&values[..3], &[0.5, 0.5, 0.5]);
        assert_eq!(values[3], 1.0, "coverage above one is one");
    }

    #[test]
    fn a_rectangle_past_a_shrunk_stage_is_clipped() {
        let patch = source([6, 0, 4, 2], stage(8, 4), |_, _| [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(
            patch.writes(stage(8, 4)),
            Region {
                x0: 6,
                y0: 0,
                width: 2,
                height: 2
            }
        );
        assert!(patch.check("Patch 1").is_ok());
        let short = PatchSource {
            rect: [0, 0, 3, 3],
            ..patch
        };
        assert_eq!(
            short.check("Patch 1").unwrap_err().kind,
            crate::ErrorKind::SourceUnavailable
        );
    }
}
