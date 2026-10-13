//! The reference renderer's banded mode (`docs/design/panorama-merge.md`, "The reference on paged
//! sources"): a stack's output evaluated in bands from windows of its source, so a source too large
//! to hold whole — a paged derived source's level 0 — is rendered, exported and analysed by the
//! reference without any stage of it ever held whole.
//!
//! # Shape
//!
//! [`Banded::new`] binds a compiled stack to a [`BandSource`] and runs the **first pass**: every
//! whole-stage statistic a window cannot compute, in stage order, each from the stage it is taken
//! from read in full-width bands through the passes before it.
//!
//! - **A spatial operation's global estimates** (the Dehaze light) are prepared from one reduction
//!   of its whole input stage, built band by band ([`build_reduction_banded`]): each block is
//!   summed from the band holding it in the one order every reduction sums in, so the estimate is
//!   the whole frame's bit for bit. The entry is then handed them, as the qualification harness
//!   hands a frame its estimates.
//! - **A repair** (Clone and Heal) evaluates its operations over the read set of every destination
//!   ([`repair_store`]), filled from its input stage rectangle by rectangle, and holds that working
//!   set for every window of its frame (`RepairEntry::held`).
//!
//! Everything else is a window. Producing a rectangle of the output — or of any stage, for a later
//! pass — walks it back through the segments ([`Windows::of`]): through each exact geometry, a
//! resample's taps with their margin ([`super::ResampleEntry::reads`]), a spatial operation's
//! summed halo, and a repair's own rectangle (its far sources are in its held working set). The
//! walk names the window of the source the first segment reads and the window of each spatial or
//! repair stage to materialize. One windowed evaluation ([`Evaluation::windowed`]) then reads that
//! source window and builds those frames in order, every pixel inside each the whole frame's: a
//! spatial tile's value does not depend on the rectangle it is cut to (`SpatialPlan::within`), and
//! the rows of the last pass are the whole render's rows over the rectangle
//! ([`super::linear::rasterize_area`]), with every colour unit handed the same coordinates. The
//! banded output is therefore the whole-frame render byte for byte, which the `reference_bands`
//! tests hold on every output family.
//!
//! # Bounds
//!
//! A rectangle whose windows — the source window, every frame and the output — would hold more
//! than [`BAND_WINDOW_BYTES`] is cut in two along its longer side, and again, until each piece
//! fits; each piece is at most [`crate::source::RENDERED_WINDOW`] of planes, refused by name past
//! it. A band is at most [`super::limits::MAX_FRAME_BYTES`] of output, the evaluated-frame limit,
//! which bounds a band rather than the frame. The first pass holds one band of a stage and the
//! reduction's grid (at most `MAX_REDUCTION_PIXELS`), or a repair's working set, which a whole
//! frame holds too. Cancellation is read between pieces, between the chunks a window reads and by
//! every pass inside a piece.
//!
//! Only the linear domain has a banded mode: a paged source is a derived source, linear planes, and
//! a JPEG is always within the camera limits.
use super::{
    Compiled, Entry, Evaluation, LinearSettings, RenderContext,
    linear::{Linear, check_resamples, rasterize_area, rasterize_area_wide},
    repair_entry::repair_store,
    spatial::{build_reduction_banded, resolve_globals},
};
use crate::{
    Cancel, Error, ProxyBounds, ProxyPlan, Raster, SnapshotId,
    artifacts::derived_source::{PagedSource, paged_source::WindowRequest},
    modules::{Parallelism, Region, Stage},
    source::RENDERED_WINDOW,
};
use std::{borrow::Cow, sync::Arc};

/// The most the windows of one piece of a banded evaluation hold together: its source window,
/// every frame it materializes and its output. A rectangle past it is cut into pieces that fit
/// ([`Banded::pieces`]). A quarter of the evaluated-frame limit's float planes, which keeps the
/// banded mode's own working set beside a paged source's 1.5 GiB chunk cache well inside the
/// process's other budgets.
pub(crate) const BAND_WINDOW_BYTES: u64 = 512 * 1024 * 1024;

/// The most rows one band of the output holds: a band of a narrow stage is limited by this rather
/// than by its bytes, so every band's evaluation still pays its halos over a useful height while
/// the export's encoder and the analysis take its rows soon.
pub(crate) const MAX_BAND_ROWS: u32 = 2048;

/// A source the banded mode reads windows of, in its content stage's coordinates: a paged derived
/// source's level 0, or a whole source's planes (for the tests that hold the two modes to each
/// other, and for any caller with a resident source).
pub(crate) trait BandSource: Sync {
    /// The content stage a stack is compiled against.
    fn dimensions(&self) -> (u32, u32);
    /// The settings its linear pixels are read under.
    fn settings(&self) -> LinearSettings;
    /// The pixels of `area`, which lies inside the content stage, in the identity view: three
    /// planes of the area's size, red row by row, then green, then blue, exactly the values a
    /// whole render reads there. `cancel` is read between the reads it is made of.
    fn window(&self, area: Region, cancel: &Cancel) -> Result<Vec<f32>, Error>;
}

/// A whole source's planes read as windows: the content stage through the source's own view.
#[cfg(test)]
pub(crate) struct WholeSource<'a> {
    pub(crate) image: &'a crate::LinearImage,
    pub(crate) settings: LinearSettings,
}

#[cfg(test)]
impl BandSource for WholeSource<'_> {
    fn dimensions(&self) -> (u32, u32) {
        (self.image.width(), self.image.height())
    }

    fn settings(&self) -> LinearSettings {
        self.settings
    }

    fn window(&self, area: Region, cancel: &Cancel) -> Result<Vec<f32>, Error> {
        let (values, plane) = RENDERED_WINDOW.layout(area.width, area.height)?;
        let mut planes = vec![0.0_f32; values];
        let reader = self.image.reader();
        for (row, y) in (area.y0..area.y1()).enumerate() {
            cancel.check()?;
            for (column, x) in (area.x0..area.x1()).enumerate() {
                let rgb = reader.pixel(x, y).ok_or_else(|| {
                    Error::internal("a source window lies outside its content stage")
                })?;
                let at = row * area.width as usize + column;
                planes[at] = rgb[0];
                planes[plane + at] = rgb[1];
                planes[2 * plane + at] = rgb[2];
            }
        }
        Ok(planes)
    }
}

/// A paged derived source's level 0, read through its chunk cache.
impl BandSource for PagedSource {
    fn dimensions(&self) -> (u32, u32) {
        (self.width(), self.height())
    }

    /// A derived source's planes are read as they are stored.
    fn settings(&self) -> LinearSettings {
        LinearSettings::default()
    }

    fn window(&self, area: Region, cancel: &Cancel) -> Result<Vec<f32>, Error> {
        let window = PagedSource::window(
            self,
            WindowRequest {
                level: 0,
                rect: [area.x0, area.y0, area.width, area.height],
                halo: 0,
            },
            cancel.flag(),
        )?;
        Ok(window.planes)
    }
}

/// What producing one rectangle of one stage reads of each stage before it ([`Windows::of`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Windows {
    /// The window of the source the first segment reads.
    pub(crate) source: Region,
    /// Per segment, the window of its input stage its spatial or repair entry materializes.
    pub(crate) areas: Vec<Option<Region>>,
}

impl Windows {
    /// The windows `rect`, a rectangle of the output stage of segment `through`, reads of every
    /// stage before it, over a `source` content stage: walked back through each segment's exact
    /// geometry and the boundary entering it, the reference renderer's own reads at each step.
    /// `O(segments)`, and reads no pixel. A resample whose reads cannot be bounded is refused by
    /// name rather than read whole.
    pub(crate) fn of(
        compiled: &Compiled,
        source: Stage,
        through: usize,
        rect: Region,
    ) -> Result<Self, Error> {
        let segments = &compiled.segments;
        let produced = segments
            .get(through)
            .ok_or_else(|| Error::internal("a banded read names a segment past its stack"))?
            .stage();
        if rect.is_empty() || rect.x1() > produced.width || rect.y1() > produced.height {
            return Err(Error::internal(
                "a banded read's rectangle lies outside its stage",
            ));
        }
        let mut areas = vec![None; segments.len()];
        let mut needed = rect;
        for index in (0..=through).rev() {
            let segment = &segments[index];
            let read = segment.geometry.unmap_region(needed);
            let received = match index.checked_sub(1) {
                Some(previous) => segments[previous].stage(),
                None => source,
            };
            if read.is_empty() || read.x1() > received.width || read.y1() > received.height {
                return Err(Error::internal(
                    "a banded read maps outside the stage its segment receives",
                ));
            }
            needed = match &segment.entry {
                None => {
                    return Ok(Self {
                        source: read,
                        areas,
                    });
                }
                Some(Entry::Spatial(entry)) => {
                    areas[index] = Some(read);
                    entry.halo_reads(read, received)
                }
                Some(Entry::Repair(_)) => {
                    areas[index] = Some(read);
                    read
                }
                Some(Entry::Resample(entry)) => entry.reads(read, received).ok_or_else(|| {
                    Error::resource_limit(
                        "the banded reference cannot bound what this stack's geometry reads of \
                         the stage before it",
                    )
                })?,
            };
        }
        Err(Error::internal(
            "a compiled stack's first segment has an entry",
        ))
    }

    /// The bytes these windows hold as float planes: the source window and every frame.
    pub(crate) fn bytes(&self) -> u64 {
        self.areas
            .iter()
            .flatten()
            .chain(std::iter::once(&self.source))
            .map(|area| area.plane_bytes())
            .sum()
    }
}

/// One stack bound to a [`BandSource`] for the reference's banded mode, its first pass run. See
/// the [module documentation](self).
pub(crate) struct Banded<'a> {
    source: &'a dyn BandSource,
    /// The stack, with every whole-stage statistic its first pass computed handed to the entry
    /// that reads it.
    compiled: Compiled,
    context: &'a RenderContext,
    /// The caller's cancellation without its progress meter: a banded evaluation's tiles are not
    /// the whole render's, so it plans and advances none.
    cancel: Cancel,
    /// What one piece's windows may hold: [`BAND_WINDOW_BYTES`] but in the tests that cut small
    /// stages into many pieces.
    budget: u64,
}

impl<'a> Banded<'a> {
    /// `compiled`, compiled against `source`'s content stage, bound to it, with its first pass
    /// run under `cancel` ([module documentation](self)). Refuses what a whole linear render of
    /// the stack refuses — more than one resample — and the first pass's own failures.
    pub(crate) fn new(
        source: &'a dyn BandSource,
        compiled: &Compiled,
        context: &'a RenderContext,
        cancel: &Cancel,
    ) -> Result<Self, Error> {
        Self::with_budget(source, compiled, context, cancel, BAND_WINDOW_BYTES)
    }

    /// [`Self::new`] with every piece held to `budget` bytes of windows.
    pub(crate) fn with_budget(
        source: &'a dyn BandSource,
        compiled: &Compiled,
        context: &'a RenderContext,
        cancel: &Cancel,
        budget: u64,
    ) -> Result<Self, Error> {
        check_resamples(compiled)?;
        let mut banded = Self {
            source,
            compiled: compiled.clone(),
            context,
            cancel: cancel.without_progress(),
            budget,
        };
        banded.first_pass()?;
        Ok(banded)
    }

    /// The output stage.
    pub(crate) fn stage(&self) -> Stage {
        self.compiled.stage()
    }

    /// The source's content stage.
    fn source_stage(&self) -> Stage {
        let (width, height) = self.source.dimensions();
        Stage { width, height }
    }

    /// The first pass: each spatial entry that reduces its stage is handed the estimates of its
    /// whole stage, and each repair the working set of its whole stage, in stage order, each read
    /// through the ones before it.
    fn first_pass(&mut self) -> Result<(), Error> {
        for index in 1..self.compiled.segments.len() {
            let received = self.compiled.segments[index - 1].stage();
            match &self.compiled.segments[index].entry {
                Some(Entry::Spatial(entry))
                    if entry.globals.is_none()
                        && entry
                            .operation
                            .units()
                            .iter()
                            .any(|unit| unit.estimate_key().is_some()) =>
                {
                    let rows = self.rows_for(index - 1, 12);
                    let reduction =
                        build_reduction_banded(received, rows, &self.cancel, |band, planes| {
                            planes.clear();
                            let (values, _) = RENDERED_WINDOW.layout(band.width, band.height)?;
                            planes.resize(values, 0.0);
                            self.fill(index - 1, band, planes)
                        })?;
                    let globals =
                        resolve_globals(self.context, &entry.operation, || Ok(reduction))?;
                    if let Some(Entry::Spatial(entry)) = &mut self.compiled.segments[index].entry {
                        entry.globals = Some(Arc::new(globals));
                    }
                }
                Some(Entry::Repair(entry)) if entry.held.is_none() => {
                    let (store, _reservation) = repair_store(
                        entry,
                        received,
                        &self.cancel,
                        self.context,
                        |rect, planes, _| self.fill(index - 1, rect, planes),
                    )?;
                    let store = Arc::new(store);
                    if let Some(Entry::Repair(entry)) = &mut self.compiled.segments[index].entry {
                        entry.held = Some(store);
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// The windows of producing `rect` of segment `through`'s output, and the bytes they hold with
    /// an output of `output` bytes a pixel.
    fn windows(&self, through: usize, rect: Region, output: u64) -> Result<(Windows, u64), Error> {
        let windows = Windows::of(&self.compiled, self.source_stage(), through, rect)?;
        let bytes = windows.bytes() + rect.pixels() * output;
        Ok((windows, bytes))
    }

    /// `rect` of segment `through`'s output cut into pieces whose windows each hold at most the
    /// budget, with an output of `output` bytes a pixel: the rectangle itself when it fits, and
    /// otherwise its halves along its longer side, cut again until each fits or is one pixel.
    /// In row-major order of the pieces' origins within each cut, which reads the source left to
    /// right down a band. `O(pieces × segments)`, and reads no pixel.
    pub(super) fn pieces(
        &self,
        through: usize,
        rect: Region,
        output: u64,
    ) -> Result<Vec<Region>, Error> {
        let mut pending = vec![rect];
        let mut pieces = Vec::new();
        while let Some(piece) = pending.pop() {
            let (_, bytes) = self.windows(through, piece, output)?;
            if bytes <= self.budget || piece.pixels() <= 1 {
                pieces.push(piece);
                continue;
            }
            let (first, second) = if piece.width >= piece.height {
                let half = piece.width / 2;
                (
                    Region {
                        width: half,
                        ..piece
                    },
                    Region {
                        x0: piece.x0 + half,
                        width: piece.width - half,
                        ..piece
                    },
                )
            } else {
                let half = piece.height / 2;
                (
                    Region {
                        height: half,
                        ..piece
                    },
                    Region {
                        y0: piece.y0 + half,
                        height: piece.height - half,
                        ..piece
                    },
                )
            };
            // The second is popped last, so the first is cut and taken first.
            pending.push(second);
            pending.push(first);
        }
        Ok(pieces)
    }

    /// How many rows of segment `through`'s output one full-width band holds, with an output of
    /// `output` bytes a pixel: the most, up to [`MAX_BAND_ROWS`] and the evaluated-frame limit,
    /// whose windows at the top of the stage fit the budget, or 64 when not even one row does, in
    /// which case each band is cut into columns. `O(log rows × segments)`, and reads no pixel.
    pub(super) fn rows_for(&self, through: usize, output: u64) -> u32 {
        self.rows_within(through, output, 4)
    }

    /// [`Self::rows_for`], with each band held to the evaluated-frame limit at `frame` bytes a
    /// pixel rather than 4: what a band of the wide terminal, at six bytes a pixel, and its 16-bit
    /// output beside it, at eight, are held to.
    pub(super) fn rows_within(&self, through: usize, output: u64, frame: u64) -> u32 {
        let stage = self.compiled.segments[through].stage();
        let frame_rows = (super::limits::MAX_FRAME_BYTES
            / (u64::from(stage.width.max(1)) * frame.max(1)))
        .clamp(1, u64::from(MAX_BAND_ROWS)) as u32;
        let most = frame_rows.min(stage.height).max(1);
        let fits = |rows: u32| {
            self.windows(
                through,
                Region {
                    x0: 0,
                    y0: 0,
                    width: stage.width,
                    height: rows,
                },
                output,
            )
            .is_ok_and(|(_, bytes)| bytes <= self.budget)
        };
        if !fits(1) {
            return most.min(64);
        }
        let (mut low, mut high) = (1, most);
        while low < high {
            let middle = low + (high - low).div_ceil(2);
            if fits(middle) {
                low = middle;
            } else {
                high = middle - 1;
            }
        }
        low
    }

    /// One windowed evaluation of the stack through segment `through`, producing `rect` of its
    /// output, handed to `read`: the source window read and every frame before `through + 1`
    /// materialized over its window, all released when `read` returns.
    fn with_window<T>(
        &self,
        through: usize,
        rect: Region,
        read: impl FnOnce(&Evaluation<'_, Linear<'_>>) -> Result<T, Error>,
    ) -> Result<T, Error> {
        self.cancel.check()?;
        let windows = Windows::of(&self.compiled, self.source_stage(), through, rect)?;
        let planes = self.source.window(windows.source, &self.cancel)?;
        let domain = Linear::window(&planes, windows.source, self.source.settings())?;
        let evaluation = Evaluation::windowed(
            domain,
            Cow::Borrowed(&self.compiled),
            windows.areas,
            through + 1,
            &self.cancel,
            self.context,
        )?;
        read(&evaluation)
    }

    /// Segment `through`'s output over `rect`, as the three `f32` planes a spatial operation
    /// reads it in, into `planes`, which holds exactly them: what a whole render's
    /// `Evaluation::fill_rows` reads there, piece by piece.
    fn fill(&self, through: usize, rect: Region, planes: &mut [f32]) -> Result<(), Error> {
        let len = rect.pixels() as usize;
        if planes.len() != 3 * len {
            return Err(Error::internal(
                "a banded fill's planes do not hold its rectangle",
            ));
        }
        for piece in self.pieces(through, rect, 12)? {
            if piece == rect {
                return self.with_window(through, rect, |evaluation| {
                    evaluation.fill_rows(through, rect, planes, Parallelism::Pool)
                });
            }
            let held = piece.pixels() as usize;
            let mut values = vec![0.0_f32; 3 * held];
            self.with_window(through, piece, |evaluation| {
                evaluation.fill_rows(through, piece, &mut values, Parallelism::Pool)
            })?;
            let width = piece.width as usize;
            for row in 0..piece.height as usize {
                let to = (piece.y0 - rect.y0) as usize * rect.width as usize
                    + row * rect.width as usize
                    + (piece.x0 - rect.x0) as usize;
                for channel in 0..3 {
                    let from = channel * held + row * width;
                    planes[channel * len + to..channel * len + to + width]
                        .copy_from_slice(&values[from..from + width]);
                }
            }
        }
        Ok(())
    }

    /// The output's terminal bytes over `rect`, RGBA row by row, exactly the whole render's
    /// there: piece by piece, each piece's rows written into place. `rect` lies inside the output
    /// stage and is at most the evaluated-frame limit.
    pub(crate) fn rgba(&self, rect: Region) -> Result<Vec<u8>, Error> {
        let bytes = rect.pixels().saturating_mul(4);
        if bytes > super::limits::MAX_FRAME_BYTES {
            return Err(Error::resource_limit(format!(
                "a band of {}x{} exceeds the {} MiB evaluated-frame limit",
                rect.width,
                rect.height,
                super::limits::MAX_FRAME_BYTES >> 20
            )));
        }
        let last = self.compiled.segments.len() - 1;
        let mut rgba = Vec::new();
        for piece in self.pieces(last, rect, 4)? {
            let rows = self.with_window(last, piece, |evaluation| {
                rasterize_area(evaluation, piece, &self.cancel, self.context)
            })?;
            if piece == rect {
                return Ok(rows);
            }
            if rgba.is_empty() {
                rgba = vec![0_u8; bytes as usize];
            }
            let stride = rect.width as usize * 4;
            let width = piece.width as usize * 4;
            for (row, values) in rows.chunks_exact(width).enumerate() {
                let to = ((piece.y0 - rect.y0) as usize + row) * stride
                    + (piece.x0 - rect.x0) as usize * 4;
                rgba[to..to + width].copy_from_slice(values);
            }
        }
        Ok(rgba)
    }

    /// The output's terminal values over `rect` encoded once at 16 bits, as the whole frame's wide
    /// terminal holds them there (`Render::wide_frame`): six bytes a pixel, red, green and blue
    /// codes in native byte order, row by row, piece by piece, each piece's rows written into
    /// place. `rect` lies inside the output stage and is at most the evaluated-frame limit at six
    /// bytes a pixel. What a banded 16-bit or resized export reads (`super::bands_output`).
    pub(crate) fn wide(&self, rect: Region) -> Result<Vec<u8>, Error> {
        const BYTES: u64 = super::linear::WIDE_TERMINAL_BYTES as u64;
        let bytes = rect.pixels().saturating_mul(BYTES);
        if bytes > super::limits::MAX_FRAME_BYTES {
            return Err(Error::resource_limit(format!(
                "a band of {}x{} exceeds the {} MiB evaluated-frame limit",
                rect.width,
                rect.height,
                super::limits::MAX_FRAME_BYTES >> 20
            )));
        }
        let last = self.compiled.segments.len() - 1;
        let mut wide = Vec::new();
        for piece in self.pieces(last, rect, BYTES)? {
            let rows = self.with_window(last, piece, |evaluation| {
                rasterize_area_wide(evaluation, piece, &self.cancel, self.context)
            })?;
            if piece == rect {
                return Ok(rows);
            }
            if wide.is_empty() {
                wide = vec![0_u8; bytes as usize];
            }
            let stride = rect.width as usize * BYTES as usize;
            let width = piece.width as usize * BYTES as usize;
            for (row, values) in rows.chunks_exact(width).enumerate() {
                let to = ((piece.y0 - rect.y0) as usize + row) * stride
                    + (piece.x0 - rect.x0) as usize * BYTES as usize;
                wide[to..to + width].copy_from_slice(values);
            }
        }
        Ok(wide)
    }

    /// How many rows each band of the output's wide terminal holds when each pixel of the band
    /// and what is made of it beside it take `frame` bytes: the most whose windows fit the budget
    /// with the wide terminal's six bytes a pixel, up to [`MAX_BAND_ROWS`] and the evaluated-frame
    /// limit at `frame` bytes a pixel.
    pub(crate) fn wide_band_rows(&self, frame: u64) -> u32 {
        let last = self.compiled.segments.len() - 1;
        self.rows_within(last, super::linear::WIDE_TERMINAL_BYTES as u64, frame)
    }

    /// The output's own linear values over `rect`, row by row, as `f32`: what the reference tile
    /// service answers a linear read with ([`crate::tiles::ReadValues::Linear`]). For the paged
    /// source's sample reads and its GPU qualification, which read it once their paths do.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn linear(&self, rect: Region) -> Result<Vec<[f32; 3]>, Error> {
        crate::Raster::expected_len(rect.width, rect.height)?;
        let last = self.compiled.segments.len() - 1;
        let mut values = vec![[0.0_f32; 3]; rect.pixels() as usize];
        for piece in self.pieces(last, rect, 24)? {
            let pixels = self.with_window(last, piece, |evaluation| {
                let mut pixels = Vec::with_capacity(piece.pixels() as usize);
                evaluation.region_in(last, piece, &mut pixels, &mut Default::default())?;
                Ok(pixels)
            })?;
            let width = piece.width as usize;
            for (row, line) in pixels.chunks_exact(width).enumerate() {
                let to = ((piece.y0 - rect.y0) as usize + row) * rect.width as usize
                    + (piece.x0 - rect.x0) as usize;
                for (value, pixel) in values[to..to + width].iter_mut().zip(line) {
                    *value = pixel.map(|channel| channel as f32);
                }
            }
        }
        Ok(values)
    }

    /// How many rows each band of the output holds ([`Self::bands`]).
    pub(crate) fn band_rows(&self) -> u32 {
        self.rows_for(self.compiled.segments.len() - 1, 4)
    }

    /// The output in full-width bands from the top, each its row count and its terminal bytes,
    /// RGBA row by row, exactly the whole render's rows: what the export's encoder and an
    /// analysis take as they come. Each band is evaluated when it is asked for; an error ends the
    /// bands.
    pub(crate) fn bands(&self) -> impl Iterator<Item = Result<(u32, Vec<u8>), Error>> + '_ {
        let stage = self.stage();
        let rows = self.band_rows();
        let mut y0 = 0;
        let mut failed = false;
        std::iter::from_fn(move || {
            if failed || y0 >= stage.height {
                return None;
            }
            let height = rows.min(stage.height - y0);
            let band = self.rgba(Region {
                x0: 0,
                y0,
                width: stage.width,
                height,
            });
            y0 += height;
            failed = band.is_err();
            Some(band.map(|rgba| (height, rgba)))
        })
    }

    /// The output reduced to fit `bounds` by the linear-light area average the reference frame is
    /// reduced by at a view smaller than the stage ([`super::reduce_to_view`]), band by band: the
    /// reduced frame of the whole render, byte for byte, without the whole frame. `None` when the
    /// output already fits. What a reduced view of a paged source's output is held to: the GPU's
    /// overview tolerance is declared against it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn reduced(
        &self,
        bounds: ProxyBounds,
        snapshot_id: SnapshotId,
        source_fingerprint: &str,
    ) -> Result<Option<Raster>, Error> {
        let stage = self.stage();
        let dimensions = (stage.width, stage.height);
        let Some(plan) = ProxyPlan::fit(dimensions, dimensions, bounds) else {
            return Ok(None);
        };
        let mut reduction = crate::proxy::RowReduction::new(dimensions, plan)?;
        for band in self.bands() {
            let (_, rgba) = band?;
            reduction.push(&rgba, &self.cancel)?;
        }
        let (width, height, rgba) = reduction.finish()?;
        Ok(Some(Raster {
            width,
            height,
            rgba: Arc::new(rgba),
            source_fingerprint: source_fingerprint.to_owned(),
            snapshot_id,
        }))
    }
}
