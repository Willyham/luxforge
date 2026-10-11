//! Deterministic synthetic RAW exposure brackets for HDR merging (`docs/design/hdr-merge.md`):
//! two to nine uncompressed 16-bit Bayer DNGs of one synthetic outdoor scene, recorded by one
//! synthetic body, `Luxforge Synthetic Bracket`, which the RAW camera catalog
//! (`crates/luxforge-raw/data/cameras.json`) admits like any DNG camera, so each file is read,
//! indexed, decoded and developed exactly as a person's DNG is. [`write_bracket`] materialises one
//! into a folder; [`Bracket`] also answers the scene's ground truth.
//!
//! The scene holds real HDR range: a sky and a lit window that clip in the metered (0 EV) frame
//! and are recorded by the shorter exposures, a sun no exposure records, a doorway in deep shadow
//! that only the longer exposures lift out of the read noise, mid-tone brick, fields and a road
//! with plenty of edges for registration, a colour chart and a grey scale. Optionally each
//! non-reference frame is turned and shifted a little, as a handheld bracket is, and a red ball
//! moves in one frame.
//!
//! **Camera model.** Radiance is linear sRGB/D65 in units of the metered frame: a grey of 1.0
//! reaches sensor white there. A frame `o` EV brighter records `2^o` times it. Camera RGB is
//! [`CAMERA`] · rgb, whose rows sum to the as-shot neutral [`NEUTRAL`] (green 1), so after the
//! development's as-shot white balance and the DNG colour matrix (`ColorMatrix1` = `ColorMatrix2` =
//! [`CAMERA`] · XYZ→sRGB, with LibRaw's sRGB primaries), an ideal development of the metered frame
//! is the radiance itself. Each Bayer site (RGGB) integrates a 2 × 2 grid of point samples over its
//! pixel, which the lens then blurs (`[1 4 6 4 1]²/256`, so hard edges do not alias and bias
//! registration), gains shot noise (one electron a DN) and read noise (2.5 DN) when asked, and is
//! rounded and clipped to `[0, WHITE]` above a black level of [`BLACK`]. The ground truth,
//! [`Bracket::truth`], is the metered frame's blurred optical image. White balance gains above one
//! mean a channel's balanced value clips at 1 before its sensor site does; [`Bracket::level`]
//! answers that balanced level.
//!
//! **Determinism.** Only arithmetic, `floor`, `round` and `sqrt` (all correctly rounded): no
//! transcendental function, so every platform writes the same bytes, and the test below pins
//! their SHA-256.
use sha2::{Digest, Sha256};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::OnceLock,
};

/// The synthetic body, as its DNGs' `Make` and `Model` and the camera catalog record it.
pub const MAKE: &str = "Luxforge";
pub const MODEL: &str = "Synthetic Bracket";
/// The sensor, which the catalog's one recording mode of this body requires exactly.
pub const SENSOR_WIDTH: u32 = 1536;
pub const SENSOR_HEIGHT: u32 = 1024;
/// The DNG default crop's origin on the sensor; the developed frame is [`WIDTH`] × [`HEIGHT`].
pub const CROP_ORIGIN: (u32, u32) = (8, 8);
pub const WIDTH: u32 = SENSOR_WIDTH - 16;
pub const HEIGHT: u32 = SENSOR_HEIGHT - 16;
pub const BLACK: u16 = 512;
pub const WHITE: u16 = 16_383;
/// The camera's response to linear sRGB: rows red, green and blue sites, each summing to its
/// [`NEUTRAL`] value.
pub const CAMERA: [[f64; 3]; 3] = [[0.40, 0.10, -0.03], [0.12, 0.80, 0.08], [0.00, 0.12, 0.56]];
/// The camera's response to a D65 white, green-normalised: the DNG `AsShotNeutral`, whose
/// inverse is the development's as-shot white balance.
pub const NEUTRAL: [f64; 3] = [0.47, 1.0, 0.68];
/// The metered frame's settings; the others differ in shutter time alone.
pub const BASE_SHUTTER_DENOMINATOR: u32 = 125;
pub const F_NUMBER: f64 = 8.0;
pub const ISO: u16 = 100;
pub const FOCAL_MM: f64 = 35.0;
pub const LENS: &str = "Synthetic 35mm f/2";
/// When the first frame was taken, as its `DateTimeOriginal` and `OffsetTimeOriginal`.
pub const CAPTURE_DAY: &str = "2026:06:21";
pub const CAPTURE_SECOND_OF_DAY: u32 = 20 * 3600 + 15 * 60 + 30;
pub const CAPTURE_OFFSET: &str = "+02:00";
/// LibRaw's sRGB → XYZ (D65) matrix, `LibRaw_constants::xyz_rgb`, which its DNG calibration
/// multiplies the colour matrix by.
const SRGB_TO_XYZ: [[f64; 3]; 3] = [
    [0.412_456_4, 0.357_576_1, 0.180_437_5],
    [0.212_672_9, 0.715_152_2, 0.072_175_0],
    [0.019_333_9, 0.119_192_0, 0.950_304_1],
];
const RANGE: f64 = (WHITE - BLACK) as f64;
const READ_NOISE_DN: f64 = 2.5;
/// The red ball's centre in the reference frame, its radius, and where it is in the frame it
/// moves in.
const SUBJECT: (f64, f64) = (905.0, 640.0);
const SUBJECT_RADIUS: f64 = 46.0;
const SUBJECT_MOVED: (f64, f64) = (1045.0, 612.0);
/// Each non-reference frame's handheld motion, in capture order after the reference: rotation in
/// radians and shift in pixels.
const HANDHELD: [(f64, f64, f64); 8] = [
    (0.0035, 3.25, -2.5),
    (-0.0026, -4.5, 1.75),
    (0.0018, 5.0, 3.0),
    (-0.0031, -2.25, -3.75),
    (0.0022, 1.5, 4.25),
    (-0.0015, -5.25, -1.0),
    (0.0029, 2.75, 2.25),
    (-0.0020, -1.75, -4.5),
];

/// What bracket to make.
#[derive(Clone, Debug, PartialEq)]
pub struct BracketSpec {
    /// 2 to 9 frames.
    pub frames: usize,
    /// Whole stops between neighbouring exposures, 1 to 3. The frames are taken metered first,
    /// then darker and brighter alternately: 0, −s, +s, −2s, +2s, …, as a camera's bracket
    /// order "0, −, +" does.
    pub step_ev: u8,
    /// The scene's texture and the noise.
    pub seed: u64,
    /// Turn and shift each frame after the first a little, as a handheld bracket.
    pub handheld: bool,
    /// The frame in which the red ball has moved, if any.
    pub moving_subject: Option<usize>,
    /// Shot and read noise.
    pub noise: bool,
    /// The first file's number: files are `LXF_0001.DNG` onwards.
    pub first_number: u32,
}

impl Default for BracketSpec {
    fn default() -> Self {
        Self {
            frames: 3,
            step_ev: 2,
            seed: 1,
            handheld: false,
            moving_subject: None,
            noise: true,
            first_number: 1,
        }
    }
}

/// A rigid motion about the developed frame's centre, from reference coordinates to the frame's,
/// `q = c + R(θ)(p − c) + t`: the merge's own convention (`RigidTransform`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rigid {
    pub rotation_rad: f64,
    pub shift: (f64, f64),
}

impl Rigid {
    pub const IDENTITY: Self = Self {
        rotation_rad: 0.0,
        shift: (0.0, 0.0),
    };

    fn centre() -> (f64, f64) {
        (
            (f64::from(WIDTH) - 1.0) / 2.0,
            (f64::from(HEIGHT) - 1.0) / 2.0,
        )
    }

    /// Where the frame recorded reference point `p`.
    pub fn map(&self, p: (f64, f64)) -> (f64, f64) {
        let (s, c) = sin_cos_small(self.rotation_rad);
        let o = Self::centre();
        let (dx, dy) = (p.0 - o.0, p.1 - o.1);
        (
            o.0 + c * dx - s * dy + self.shift.0,
            o.1 + s * dx + c * dy + self.shift.1,
        )
    }

    /// The reference point a frame's pixel `q` recorded.
    pub fn unmap(&self, q: (f64, f64)) -> (f64, f64) {
        let (s, c) = sin_cos_small(self.rotation_rad);
        let o = Self::centre();
        let (dx, dy) = (q.0 - self.shift.0 - o.0, q.1 - self.shift.1 - o.1);
        (o.0 + c * dx + s * dy, o.1 - s * dx + c * dy)
    }
}

/// One frame of a bracket.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub file_name: String,
    /// Stops brighter than the reference (frame 0): its `ExposureBiasValue`, and the shutter
    /// time's ratio to the reference's.
    pub offset_ev: i32,
    /// `ExposureTime` as a rational, seconds.
    pub exposure_time: (u32, u32),
    /// Milliseconds after the first frame's capture.
    pub captured_after_ms: u32,
    pub motion: Rigid,
    /// Where the ball is, in this frame's own coordinates' scene (reference coordinates).
    pub subject: (f64, f64),
}

impl Frame {
    pub fn exposure_time_s(&self) -> f64 {
        f64::from(self.exposure_time.0) / f64::from(self.exposure_time.1)
    }
}

/// A bracket: its frames' metadata, their DNG bytes and the scene's ground truth.
pub struct Bracket {
    spec: BracketSpec,
    frames: Vec<Frame>,
    /// The metered frame's optical image on the sensor, made once when first asked for.
    reference: OnceLock<Vec<[f64; 3]>>,
}

/// Write `spec`'s bracket into `dir` (made if missing): one DNG a frame, in capture order.
pub fn write_bracket(dir: &Path, spec: &BracketSpec) -> io::Result<Vec<PathBuf>> {
    Bracket::new(spec).write(dir)
}

impl Bracket {
    /// # Panics
    /// When `spec` asks for fewer than 2 or more than 9 frames, a step outside 1–3 stops, or a
    /// moving subject in no frame.
    pub fn new(spec: &BracketSpec) -> Self {
        assert!((2..=9).contains(&spec.frames), "2 to 9 frames");
        assert!((1..=3).contains(&spec.step_ev), "a step of 1 to 3 stops");
        assert!(
            spec.moving_subject.is_none_or(|frame| frame < spec.frames),
            "the moving subject's frame is in the bracket"
        );
        let mut frames = Vec::with_capacity(spec.frames);
        let mut after_ms = 0;
        for at in 0..spec.frames {
            let rank = at.div_ceil(2) as i32;
            let offset_ev = if at % 2 == 1 { -rank } else { rank } * i32::from(spec.step_ev);
            let exposure_time = if offset_ev >= 0 {
                (1 << offset_ev, BASE_SHUTTER_DENOMINATOR)
            } else {
                (1, BASE_SHUTTER_DENOMINATOR << -offset_ev)
            };
            let motion = if spec.handheld && at > 0 {
                let (rotation_rad, x, y) = HANDHELD[at - 1];
                Rigid {
                    rotation_rad,
                    shift: (x, y),
                }
            } else {
                Rigid::IDENTITY
            };
            let frame = Frame {
                file_name: format!("LXF_{:04}.DNG", spec.first_number + at as u32),
                offset_ev,
                exposure_time,
                captured_after_ms: after_ms,
                motion,
                subject: if spec.moving_subject == Some(at) {
                    SUBJECT_MOVED
                } else {
                    SUBJECT
                },
            };
            // The next frame follows this one's exposure and a quarter-second of readout.
            after_ms += (1000 * frame.exposure_time.0).div_ceil(frame.exposure_time.1) + 250;
            frames.push(frame);
        }
        Self {
            spec: spec.clone(),
            frames,
            reference: OnceLock::new(),
        }
    }

    pub fn spec(&self) -> &BracketSpec {
        &self.spec
    }

    /// The frames in capture order; the first is the metered reference.
    pub fn frames(&self) -> &[Frame] {
        &self.frames
    }

    /// Write every frame into `dir`, made if missing: the paths, in capture order.
    pub fn write(&self, dir: &Path) -> io::Result<Vec<PathBuf>> {
        fs::create_dir_all(dir)?;
        let mut paths = Vec::with_capacity(self.frames.len());
        for (at, frame) in self.frames.iter().enumerate() {
            let path = dir.join(&frame.file_name);
            fs::write(&path, self.dng(at))?;
            paths.push(path);
        }
        Ok(paths)
    }

    /// The SHA-256 of every frame's bytes, in order, as lowercase hex.
    pub fn sha256(&self) -> String {
        let mut digest = Sha256::new();
        for at in 0..self.frames.len() {
            digest.update(self.dng(at));
        }
        digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// The scene's linear sRGB radiance at reference point `(x, y)` (developed pixel
    /// coordinates, pixel centres at integers), in units of the metered exposure, before the
    /// lens: the sharp scene.
    pub fn radiance(&self, x: f64, y: f64) -> [f64; 3] {
        scene(self.spec.seed, x, y, SUBJECT)
    }

    /// The ground truth of reference pixel `(x, y)` (`x < WIDTH`, `y < HEIGHT`): the optical
    /// image the metered frame's site there integrates (the scene through the lens's blur), what
    /// an ideal, noise-free and unclipped development of that frame holds there, and what a
    /// merge's result holds at the same reference pixel.
    pub fn truth(&self, x: u32, y: u32) -> [f64; 3] {
        let site = (y + CROP_ORIGIN.1) * SENSOR_WIDTH + x + CROP_ORIGIN.0;
        self.reference_image()[site as usize]
    }

    /// The largest white-balanced camera channel frame `at` recorded reference pixel `(x, y)` at,
    /// in units of the level at which that channel clips (sensor white, or the balanced 1 a gain
    /// above one reaches first): at or above 1 the frame clipped it. A moved ball is not
    /// accounted for.
    pub fn level(&self, at: usize, x: u32, y: u32) -> f64 {
        let gain = exp2i(self.frames[at].offset_ev);
        let rgb = self.truth(x, y);
        CAMERA
            .iter()
            .zip(NEUTRAL)
            .map(|(row, neutral)| {
                gain * (row[0] * rgb[0] + row[1] * rgb[1] + row[2] * rgb[2]) / neutral
            })
            .fold(0.0, f64::max)
    }

    /// Whether `(x, y)` is within `margin` pixels of where the ball is in any frame, where a
    /// merge's result may show either position.
    pub fn near_subject(&self, x: f64, y: f64, margin: f64) -> bool {
        self.frames.iter().any(|frame| {
            let (dx, dy) = (x - frame.subject.0, y - frame.subject.1);
            let reach = SUBJECT_RADIUS + margin;
            dx * dx + dy * dy <= reach * reach
        })
    }

    /// The one-sigma noise, per channel and in units of the metered exposure, of the mean of a
    /// flat block of `pixels` developed pixels of radiance `rgb` that frame `at` recorded: its
    /// sites' shot, read and rounding noise (`pixels / 4` red and blue sites, `pixels / 2` green),
    /// carried through the colour matrix. Zero without noise. Demosaicing spreads a site over its
    /// neighbours without changing how many sites a block's mean rests on, so this holds to within
    /// the interpolation's reach past the block's edge.
    pub fn block_sigma(&self, at: usize, rgb: [f64; 3], pixels: u32) -> [f64; 3] {
        if !self.spec.noise {
            return [0.0; 3];
        }
        let scale = exp2i(self.frames[at].offset_ev) * RANGE;
        let sites = [0.25, 0.5, 0.25].map(|share| share * f64::from(pixels));
        let variance: Vec<f64> = CAMERA
            .iter()
            .zip(sites)
            .map(|(row, sites)| {
                let electrons = scale * (row[0] * rgb[0] + row[1] * rgb[1] + row[2] * rgb[2]);
                let dn = electrons.max(0.0) + READ_NOISE_DN * READ_NOISE_DN + 1.0 / 12.0;
                dn / sites / (scale * scale)
            })
            .collect();
        let inverse = inverse(&CAMERA);
        inverse.map(|row| {
            (0..3)
                .map(|j| row[j] * row[j] * variance[j])
                .sum::<f64>()
                .sqrt()
        })
    }

    fn reference_image(&self) -> &[[f64; 3]] {
        self.reference.get_or_init(|| self.optical_image(0))
    }

    /// The optical image frame `at` records on its sensor, `[r, g, b]` per site, row by row over
    /// the whole sensor: each site's 2 × 2 samples of the scene where the frame looked, then the
    /// lens's blur.
    fn optical_image(&self, at: usize) -> Vec<[f64; 3]> {
        blur(&self.sharp_image(at))
    }

    /// Each sensor site's sample-averaged radiance as frame `at` records it, in its own pixel
    /// grid, before the lens's blur.
    fn sharp_image(&self, at: usize) -> Vec<[f64; 3]> {
        let frame = &self.frames[at];
        let (ox, oy) = (f64::from(CROP_ORIGIN.0), f64::from(CROP_ORIGIN.1));
        let mut grid = Vec::with_capacity((SENSOR_WIDTH * SENSOR_HEIGHT) as usize);
        for sy in 0..SENSOR_HEIGHT {
            for sx in 0..SENSOR_WIDTH {
                let (qx, qy) = (f64::from(sx) - ox, f64::from(sy) - oy);
                let mut sum = [0.0; 3];
                for (dx, dy) in SUBSAMPLES {
                    let (px, py) = frame.motion.unmap((qx + dx, qy + dy));
                    let rgb = scene(self.spec.seed, px, py, frame.subject);
                    for (total, value) in sum.iter_mut().zip(rgb) {
                        *total += value;
                    }
                }
                grid.push(sum.map(|total| total / 4.0));
            }
        }
        grid
    }

    /// Frame `at`'s DNG bytes, reusing the reference's optical image when the frame records the
    /// same scene at the same place.
    pub fn dng(&self, at: usize) -> Vec<u8> {
        let frame = &self.frames[at];
        let own;
        let grid =
            if frame.motion == self.frames[0].motion && frame.subject == self.frames[0].subject {
                self.reference_image()
            } else {
                own = self.optical_image(at);
                &own
            };
        let gain = exp2i(frame.offset_ev);
        let mut samples = Vec::with_capacity(grid.len() * 2);
        for (site, rgb) in grid.iter().enumerate() {
            let (sx, sy) = (site as u32 % SENSOR_WIDTH, site as u32 / SENSOR_WIDTH);
            // RGGB from the sensor's origin.
            let channel = match (sy % 2, sx % 2) {
                (0, 0) => 0,
                (1, 1) => 2,
                _ => 1,
            };
            let row = CAMERA[channel];
            let camera = row[0] * rgb[0] + row[1] * rgb[1] + row[2] * rgb[2];
            let mut electrons = gain * camera * RANGE;
            if self.spec.noise {
                let mut noise = Noise::new(self.spec.seed, at as u64, site as u64);
                electrons +=
                    electrons.max(0.0).sqrt() * noise.gaussian() + READ_NOISE_DN * noise.gaussian();
            }
            let value = (f64::from(BLACK) + electrons)
                .round()
                .clamp(0.0, f64::from(WHITE)) as u16;
            samples.extend_from_slice(&value.to_le_bytes());
        }
        self.container(at, samples)
    }

    /// Frame `at`'s TIFF/DNG container around its samples: the raw CFA image in IFD0 with the
    /// DNG tags, and an Exif IFD with its exposure and capture time.
    fn container(&self, at: usize, samples: Vec<u8>) -> Vec<u8> {
        let frame = &self.frames[at];
        let second = CAPTURE_SECOND_OF_DAY + frame.captured_after_ms / 1000;
        let time = format!(
            "{CAPTURE_DAY} {:02}:{:02}:{:02}",
            second / 3600,
            second / 60 % 60,
            second % 60
        );
        let subsec = format!("{:03}", frame.captured_after_ms % 1000);
        let matrix = colour_matrix();
        let fixed = |value: f64| ((value * 1e6).round() as i32, 1_000_000);
        let matrix: Vec<(i32, i32)> = matrix.iter().flatten().map(|v| fixed(*v)).collect();
        let neutral: Vec<(u32, u32)> = NEUTRAL
            .iter()
            .map(|value| ((value * 100.0).round() as u32, 100))
            .collect();
        let mut exif = vec![
            Tag::rational(33_434, &[frame.exposure_time]),
            Tag::rational(33_437, &[((F_NUMBER * 10.0) as u32, 10)]),
            Tag::short(34_855, &[ISO]),
            Tag::undefined(36_864, b"0232"),
            Tag::ascii(36_867, &time),
            Tag::ascii(36_868, &time),
            Tag::ascii(36_881, CAPTURE_OFFSET),
            Tag::srational(37_380, &[(frame.offset_ev, 1)]),
            Tag::rational(37_386, &[((FOCAL_MM * 10.0) as u32, 10)]),
            Tag::ascii(37_521, &subsec),
            Tag::ascii(42_036, LENS),
        ];
        let mut ifd0 = vec![
            Tag::long(254, &[0]),
            Tag::long(256, &[SENSOR_WIDTH]),
            Tag::long(257, &[SENSOR_HEIGHT]),
            Tag::short(258, &[16]),
            Tag::short(259, &[1]),
            Tag::short(262, &[32_803]),
            Tag::ascii(271, MAKE),
            Tag::ascii(272, MODEL),
            Tag::long(273, &[0]),
            Tag::short(274, &[1]),
            Tag::short(277, &[1]),
            Tag::long(278, &[SENSOR_HEIGHT]),
            Tag::long(279, &[samples.len() as u32]),
            Tag::short(284, &[1]),
            Tag::ascii(305, "luxforge-testkit hdr_bracket"),
            Tag::ascii(306, &time),
            Tag::short(33_421, &[2, 2]),
            Tag::byte(33_422, &[0, 1, 1, 2]),
            Tag::long(34_665, &[0]),
            Tag::byte(50_706, &[1, 4, 0, 0]),
            Tag::byte(50_707, &[1, 1, 0, 0]),
            Tag::ascii(50_708, &format!("{MAKE} {MODEL}")),
            Tag::byte(50_710, &[0, 1, 2]),
            Tag::short(50_711, &[1]),
            Tag::short(50_713, &[1, 1]),
            Tag::long(50_714, &[u32::from(BLACK)]),
            Tag::long(50_717, &[u32::from(WHITE)]),
            Tag::long(50_719, &[CROP_ORIGIN.0, CROP_ORIGIN.1]),
            Tag::long(50_720, &[WIDTH, HEIGHT]),
            Tag::srational(50_721, &matrix),
            Tag::srational(50_722, &matrix),
            Tag::rational(50_728, &neutral),
            Tag::short(50_778, &[17]),
            Tag::short(50_779, &[21]),
        ];
        ifd0.sort_by_key(|tag| tag.tag);
        exif.sort_by_key(|tag| tag.tag);
        let exif_at = 8 + ifd_bytes(&ifd0);
        let strip_at = (exif_at + ifd_bytes(&exif)).next_multiple_of(16);
        for tag in &mut ifd0 {
            match tag.tag {
                273 => tag.data = (strip_at as u32).to_le_bytes().to_vec(),
                34_665 => tag.data = (exif_at as u32).to_le_bytes().to_vec(),
                _ => {}
            }
        }
        let mut out = Vec::with_capacity(strip_at + samples.len());
        out.extend_from_slice(b"II*\0\x08\0\0\0");
        write_ifd(&mut out, &ifd0);
        write_ifd(&mut out, &exif);
        out.resize(strip_at, 0);
        out.extend_from_slice(&samples);
        out
    }
}

/// The 2 × 2 point samples a pixel integrates, about its centre.
const SUBSAMPLES: [(f64, f64); 4] = [(-0.25, -0.25), (0.25, -0.25), (-0.25, 0.25), (0.25, 0.25)];

/// The lens: a separable binomial blur `[1 4 6 4 1] / 16` (a Gaussian of about one pixel's
/// deviation), edges clamped. It keeps the scene's hard edges from aliasing on the sensor, as a
/// real lens and filter stack do: registration on an aliased frame is biased by a sub-pixel phase
/// the frames do not share. Nearly isotropic, so a frame turned by a fraction of a degree sees the
/// same blur.
fn blur(image: &[[f64; 3]]) -> Vec<[f64; 3]> {
    const TAPS: [f64; 5] = [1.0, 4.0, 6.0, 4.0, 1.0];
    let (w, h) = (i64::from(SENSOR_WIDTH), i64::from(SENSOR_HEIGHT));
    let pass = |source: &[[f64; 3]], across: bool| -> Vec<[f64; 3]> {
        let mut out = Vec::with_capacity(source.len());
        for y in 0..h {
            for x in 0..w {
                let mut sum = [0.0; 3];
                for (k, tap) in TAPS.iter().enumerate() {
                    let d = k as i64 - 2;
                    let (sx, sy) = if across {
                        ((x + d).clamp(0, w - 1), y)
                    } else {
                        (x, (y + d).clamp(0, h - 1))
                    };
                    let rgb = source[(sy * w + sx) as usize];
                    for (total, value) in sum.iter_mut().zip(rgb) {
                        *total += tap * value;
                    }
                }
                out.push(sum.map(|total| total / 16.0));
            }
        }
        out
    };
    pass(&pass(image, true), false)
}

/// The DNG colour matrix, XYZ → camera: [`CAMERA`] · (sRGB → XYZ)⁻¹, so LibRaw's colour matrix ·
/// sRGB → XYZ is [`CAMERA`] again.
fn colour_matrix() -> [[f64; 3]; 3] {
    let inverse = inverse(&SRGB_TO_XYZ);
    std::array::from_fn(|r| {
        std::array::from_fn(|c| (0..3).map(|k| CAMERA[r][k] * inverse[k][c]).sum())
    })
}

/// The inverse of `m`, by its adjugate.
fn inverse(m: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    std::array::from_fn(|r| {
        std::array::from_fn(|c| {
            let (r1, r2) = ((c + 1) % 3, (c + 2) % 3);
            let (c1, c2) = ((r + 1) % 3, (r + 2) % 3);
            (m[r1][c1] * m[r2][c2] - m[r1][c2] * m[r2][c1]) / det
        })
    })
}

/// `2^e` for a whole `e`, exactly.
fn exp2i(e: i32) -> f64 {
    let mut value = 1.0;
    for _ in 0..e.unsigned_abs() {
        value *= 2.0;
    }
    if e < 0 { 1.0 / value } else { value }
}

/// `sin` and `cos` of an angle of at most a few hundredths of a radian, by their Taylor series to
/// well under 1e-12: arithmetic, so every platform agrees.
fn sin_cos_small(angle: f64) -> (f64, f64) {
    let a2 = angle * angle;
    let sin = angle * (1.0 - a2 / 6.0 * (1.0 - a2 / 20.0 * (1.0 - a2 / 42.0)));
    let cos = 1.0 - a2 / 2.0 * (1.0 - a2 / 12.0 * (1.0 - a2 / 30.0));
    (sin, cos)
}

/// A 64-bit mix of `values` (SplitMix64's finaliser), for lattice values and noise.
fn mix(mut state: u64, values: &[u64]) -> u64 {
    for value in values {
        state = state.wrapping_add(value.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        state = (state ^ (state >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        state = (state ^ (state >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        state ^= state >> 31;
    }
    state
}

fn unit(bits: u64) -> f64 {
    (bits >> 11) as f64 / (1_u64 << 53) as f64
}

/// One site's noise: independent of the order sites are visited in.
struct Noise(u64);

impl Noise {
    fn new(seed: u64, frame: u64, site: u64) -> Self {
        Self(mix(0x6E6F_6973_6500_0000 ^ seed, &[frame, site]))
    }

    fn uniform(&mut self) -> f64 {
        self.0 = mix(self.0, &[1]);
        unit(self.0)
    }

    /// A zero-mean, unit-variance approximately normal deviate: four uniforms summed (Irwin–Hall),
    /// centred and scaled.
    fn gaussian(&mut self) -> f64 {
        let sum = self.uniform() + self.uniform() + self.uniform() + self.uniform();
        (sum - 2.0) * 1.732_050_807_568_877_2
    }
}

/// Smooth value noise in `[0, 1)` on a unit lattice.
fn value_noise(seed: u64, x: f64, y: f64) -> f64 {
    let (fx, fy) = (x.floor(), y.floor());
    let (tx, ty) = (x - fx, y - fy);
    let (sx, sy) = (tx * tx * (3.0 - 2.0 * tx), ty * ty * (3.0 - 2.0 * ty));
    let (ix, iy) = (fx as i64, fy as i64);
    let at = |dx: i64, dy: i64| unit(mix(seed, &[(ix + dx) as u64, (iy + dy) as u64]));
    let top = at(0, 0) + (at(1, 0) - at(0, 0)) * sx;
    let bottom = at(0, 1) + (at(1, 1) - at(0, 1)) * sx;
    top + (bottom - top) * sy
}

/// Three octaves of value noise, in `[0, 1)`.
fn fbm(seed: u64, x: f64, y: f64) -> f64 {
    0.5 * value_noise(seed, x, y)
        + 0.3 * value_noise(seed ^ 0x51, 2.03 * x + 17.0, 2.03 * y)
        + 0.2 * value_noise(seed ^ 0xa2, 4.11 * x, 4.11 * y + 31.0)
}

fn smoothstep(edge0: f64, edge1: f64, x: f64) -> f64 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn mix3(a: [f64; 3], b: [f64; 3], t: f64) -> [f64; 3] {
    std::array::from_fn(|c| a[c] + (b[c] - a[c]) * t)
}

fn scale(rgb: [f64; 3], by: f64) -> [f64; 3] {
    rgb.map(|value| value * by)
}

/// Inside the half-open rectangle `[x0, x1) × [y0, y1)`.
fn inside(x: f64, y: f64, (x0, y0, x1, y1): (f64, f64, f64, f64)) -> bool {
    x >= x0 && x < x1 && y >= y0 && y < y1
}

/// The colour chart's patches, linear sRGB: six colours, then six greys.
const CHART: [[f64; 3]; 12] = [
    [0.17, 0.08, 0.05],
    [0.55, 0.30, 0.22],
    [0.12, 0.19, 0.33],
    [0.10, 0.15, 0.05],
    [0.23, 0.21, 0.42],
    [0.13, 0.52, 0.40],
    [0.90, 0.90, 0.90],
    [0.59, 0.59, 0.59],
    [0.36, 0.36, 0.36],
    [0.19, 0.19, 0.19],
    [0.09, 0.09, 0.09],
    [0.031, 0.031, 0.031],
];
const CHART_ORIGIN: (f64, f64) = (1050.0, 770.0);
const PATCH: f64 = 62.0;
const PATCH_GAP: f64 = 10.0;

/// The scene's radiance at reference point `(x, y)` with the ball at `subject`: defined on the
/// whole plane, since a turned or shifted frame sees past the reference's edges.
fn scene(seed: u64, x: f64, y: f64, subject: (f64, f64)) -> [f64; 3] {
    let (w, h) = (f64::from(WIDTH), f64::from(HEIGHT));
    let u = x / w;
    // The ball, in front of everything.
    let (bx, by) = (x - subject.0, y - subject.1);
    let r2 = (bx * bx + by * by) / (SUBJECT_RADIUS * SUBJECT_RADIUS);
    if r2 < 1.0 {
        // Lit from the upper left, with a rim falling off towards its edge.
        let light = 0.55 + 0.45 * (1.0 - r2) - 0.25 * (bx + by) / (2.0 * SUBJECT_RADIUS);
        return scale([0.52, 0.06, 0.04], light.max(0.2));
    }
    // The building on the left, in front of the landscape.
    if (70.0..610.0).contains(&x) && y >= 0.28 * h {
        return building(seed, x, y, h);
    }
    // The chart, standing in the field.
    let (cx, cy) = (x - CHART_ORIGIN.0, y - CHART_ORIGIN.1);
    let pitch = PATCH + PATCH_GAP;
    if cx >= -PATCH_GAP && cx < 6.0 * pitch && cy >= -PATCH_GAP && cy < 2.0 * pitch {
        let (column, row) = ((cx / pitch).floor(), (cy / pitch).floor());
        let (px, py) = (cx - column * pitch, cy - row * pitch);
        if column >= 0.0 && row >= 0.0 && px < PATCH && py < PATCH {
            return CHART[(row * 6.0 + column) as usize];
        }
        return [0.012, 0.012, 0.012];
    }
    // Rolling hills on the horizon.
    let horizon = h * (0.47 + 0.05 * value_noise(seed ^ 0x4111, u * 4.0, 0.5) - 0.03 * u);
    if y < horizon {
        sky(seed, x, y, horizon)
    } else {
        ground(seed, x, y, horizon)
    }
}

/// The sky: brightest overhead, with clouds and a sun no exposure records.
fn sky(seed: u64, x: f64, y: f64, horizon: f64) -> [f64; 3] {
    let t = (y / horizon).clamp(0.0, 1.0);
    let level = 1.35 + 1.45 * (1.0 - t) * (1.0 - t);
    let blue = mix3([0.55, 0.78, 1.0], [0.85, 0.88, 0.95], t);
    let clouds = fbm(seed ^ 0xc10d, x / 170.0, y / 70.0);
    let cover = smoothstep(0.52, 0.72, clouds);
    let cloud = scale([0.95, 0.95, 0.97], (level * (0.82 + 0.3 * clouds)).min(3.0));
    let mut rgb = mix3(scale(blue, level), cloud, cover);
    let (sx, sy) = (x - 1240.0, y - 120.0);
    let d2 = sx * sx + sy * sy;
    if d2 < 18.0 * 18.0 {
        return [90.0, 86.0, 76.0];
    }
    // The sun's glow, too bright for every frame close to it.
    let d = d2.sqrt();
    if d < 90.0 {
        let glow = 1.0 - d / 90.0;
        rgb = std::array::from_fn(|c| rgb[c] + 9.0 * glow * glow * [1.0, 0.95, 0.85][c]);
    }
    rgb
}

/// Fields below the horizon, rows of crops converging, and a road with a dashed line.
fn ground(seed: u64, x: f64, y: f64, horizon: f64) -> [f64; 3] {
    let h = f64::from(HEIGHT);
    let depth = ((y - horizon) / (h - horizon)).clamp(0.0, 1.0);
    if y > 0.86 * h {
        // The road: grey asphalt with grit and a dashed centre line.
        let grit = 0.85 + 0.3 * value_noise(seed ^ 0x7a4d, x / 3.0, y / 3.0);
        if (y - 0.93 * h).abs() < 4.0 && (x / 90.0).floor() as i64 % 2 == 0 {
            return scale([0.62, 0.60, 0.52], grit);
        }
        return scale([0.11, 0.11, 0.105], grit);
    }
    // Furrows converge on a vanishing point on the horizon.
    let lane = (x - 1100.0) / (0.15 + depth);
    let furrow = 0.75 + 0.25 * (((lane / 38.0).floor() as i64).rem_euclid(2)) as f64;
    let texture = fbm(seed ^ 0x9e1d, x / 35.0, y / 18.0);
    let field = mix3(
        [0.07, 0.11, 0.035],
        [0.16, 0.13, 0.06],
        fbm(seed ^ 0xf1e1, x / 400.0, y / 150.0),
    );
    scale(
        field,
        furrow * (0.55 + 0.9 * texture) * (0.75 + 0.5 * depth),
    )
}

/// A brick wall with a lit window and a doorway in deep shadow.
fn building(seed: u64, x: f64, y: f64, h: f64) -> [f64; 3] {
    // The window: a lit interior behind dark glazing bars.
    let window = (150.0, 0.40 * h, 340.0, 0.62 * h);
    if inside(x, y, window) {
        let (wx, wy) = (x - window.0, y - window.1);
        let (ww, wh) = (window.2 - window.0, window.3 - window.1);
        let bar = |at: f64, span: f64, parts: f64| {
            let cell = span / parts;
            let into = at - (at / cell).floor() * cell;
            into < 4.0 || into > cell - 4.0
        };
        if bar(wx, ww, 3.0) || bar(wy, wh, 2.0) {
            return [0.035, 0.03, 0.025];
        }
        let lamp =
            1.6 + 0.7 * (1.0 - wy / wh) + 0.2 * value_noise(seed ^ 0x1a3b, wx / 25.0, wy / 25.0);
        return scale([1.0, 0.78, 0.52], lamp);
    }
    // The doorway: deep shadow with faint planks.
    let door = (400.0, 0.62 * h, 525.0, 0.97 * h);
    if inside(x, y, door) {
        let plank = if ((x - door.0) / 21.0).floor() as i64 % 2 == 0 {
            1.0
        } else {
            0.75
        };
        let grain = 0.6 + 0.8 * fbm(seed ^ 0xd002, x / 6.0, y / 40.0);
        return scale([0.009, 0.0075, 0.006], plank * grain);
    }
    // Bricks 48 × 20 in stretcher bond, with lighter mortar; the wall darkens under the eaves.
    let course = (y / 20.0).floor();
    let offset = if course as i64 % 2 == 0 { 0.0 } else { 24.0 };
    let column = ((x + offset) / 48.0).floor();
    let (into_x, into_y) = (x + offset - column * 48.0, y - course * 20.0);
    let shade = 0.55 + 0.45 * smoothstep(0.28 * h, 0.40 * h, y);
    if into_x < 3.0 || into_y < 3.0 {
        return scale([0.30, 0.29, 0.27], shade);
    }
    let brick = unit(mix(
        seed ^ 0xb41c,
        &[column as i64 as u64, course as i64 as u64],
    ));
    let speckle = 0.9 + 0.2 * value_noise(seed ^ 0x5bec, x / 4.0, y / 4.0);
    scale([0.24, 0.11, 0.065], shade * (0.75 + 0.5 * brick) * speckle)
}

/// One TIFF directory entry: its tag, type, count and value bytes.
struct Tag {
    tag: u16,
    kind: u16,
    count: u32,
    data: Vec<u8>,
}

impl Tag {
    fn byte(tag: u16, values: &[u8]) -> Self {
        Self {
            tag,
            kind: 1,
            count: values.len() as u32,
            data: values.to_vec(),
        }
    }

    fn ascii(tag: u16, text: &str) -> Self {
        let mut data = text.as_bytes().to_vec();
        data.push(0);
        Self {
            tag,
            kind: 2,
            count: data.len() as u32,
            data,
        }
    }

    fn short(tag: u16, values: &[u16]) -> Self {
        Self {
            tag,
            kind: 3,
            count: values.len() as u32,
            data: values.iter().flat_map(|v| v.to_le_bytes()).collect(),
        }
    }

    fn long(tag: u16, values: &[u32]) -> Self {
        Self {
            tag,
            kind: 4,
            count: values.len() as u32,
            data: values.iter().flat_map(|v| v.to_le_bytes()).collect(),
        }
    }

    fn rational(tag: u16, values: &[(u32, u32)]) -> Self {
        Self {
            tag,
            kind: 5,
            count: values.len() as u32,
            data: values
                .iter()
                .flat_map(|(n, d)| [n.to_le_bytes(), d.to_le_bytes()].concat())
                .collect(),
        }
    }

    fn undefined(tag: u16, values: &[u8]) -> Self {
        Self {
            kind: 7,
            ..Self::byte(tag, values)
        }
    }

    fn srational(tag: u16, values: &[(i32, i32)]) -> Self {
        Self {
            tag,
            kind: 10,
            count: values.len() as u32,
            data: values
                .iter()
                .flat_map(|(n, d)| [n.to_le_bytes(), d.to_le_bytes()].concat())
                .collect(),
        }
    }
}

/// A directory's bytes: its count, entries and next-IFD offset, then each value longer than four
/// bytes, word-aligned.
fn ifd_bytes(tags: &[Tag]) -> usize {
    2 + 12 * tags.len()
        + 4
        + tags
            .iter()
            .filter(|tag| tag.data.len() > 4)
            .map(|tag| tag.data.len().next_multiple_of(2))
            .sum::<usize>()
}

/// Append `tags` (sorted) as one directory at `out`'s end, with no next directory.
fn write_ifd(out: &mut Vec<u8>, tags: &[Tag]) {
    let start = out.len();
    let mut payload = start + 2 + 12 * tags.len() + 4;
    let mut tail = Vec::new();
    out.extend_from_slice(&(tags.len() as u16).to_le_bytes());
    for tag in tags {
        out.extend_from_slice(&tag.tag.to_le_bytes());
        out.extend_from_slice(&tag.kind.to_le_bytes());
        out.extend_from_slice(&tag.count.to_le_bytes());
        if tag.data.len() > 4 {
            out.extend_from_slice(&(payload as u32).to_le_bytes());
            let padded = tag.data.len().next_multiple_of(2);
            tail.extend_from_slice(&tag.data);
            tail.resize(tail.len() + padded - tag.data.len(), 0);
            payload += padded;
        } else {
            let mut inline = [0_u8; 4];
            inline[..tag.data.len()].copy_from_slice(&tag.data);
            out.extend_from_slice(&inline);
        }
    }
    out.extend_from_slice(&0_u32.to_le_bytes());
    out.extend_from_slice(&tail);
    debug_assert_eq!(out.len() - start, ifd_bytes(tags));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default bracket's bytes are pinned, so a change to the generator is deliberate; its
    /// exposures, times and files are what its specification says.
    #[test]
    fn hdr_bracket_default_is_deterministic_and_pinned() {
        let bracket = Bracket::new(&BracketSpec::default());
        let offsets: Vec<i32> = bracket
            .frames()
            .iter()
            .map(|frame| frame.offset_ev)
            .collect();
        assert_eq!(offsets, [0, -2, 2]);
        let times: Vec<(u32, u32)> = bracket
            .frames()
            .iter()
            .map(|frame| frame.exposure_time)
            .collect();
        assert_eq!(times, [(1, 125), (1, 500), (4, 125)]);
        let after: Vec<u32> = bracket
            .frames()
            .iter()
            .map(|frame| frame.captured_after_ms)
            .collect();
        assert_eq!(after, [0, 258, 510]);
        assert_eq!(
            bracket.sha256(),
            "234982072f6724a1cf63db44c4638d6f09a04e11cc39edf2d92d2ab5c613a045"
        );
    }

    /// The scene clips in the metered frame where the shortest exposure records it, holds shadow
    /// that only the longest lifts, and a sun no frame records.
    #[test]
    fn hdr_bracket_scene_spans_the_bracket() {
        let bracket = Bracket::new(&BracketSpec::default());
        let (mut clipped_recovered, mut deep, mut sun) = (0, 0, 0);
        for y in (0..HEIGHT).step_by(4) {
            for x in (0..WIDTH).step_by(4) {
                let levels: Vec<f64> = (0..3).map(|at| bracket.level(at, x, y)).collect();
                if levels[0] >= 1.0 && levels[1] < 0.8 {
                    clipped_recovered += 1;
                }
                if levels[0] < 1.0 / 64.0 && levels[2] > 1.0 / 64.0 {
                    deep += 1;
                }
                if levels.iter().all(|level| *level >= 1.0) {
                    sun += 1;
                }
            }
        }
        let pixels = (WIDTH / 4) * (HEIGHT / 4);
        assert!(
            clipped_recovered > pixels / 10,
            "{clipped_recovered} of {pixels}"
        );
        assert!(deep > 500, "{deep}");
        assert!(sun > 50 && sun < 2000, "{sun}");
    }
}
