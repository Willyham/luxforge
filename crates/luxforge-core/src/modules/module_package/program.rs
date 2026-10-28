//! An external module's GPU program as the host reports it, and what its first-use probe holds it
//! to (`docs/design/module-manager.md#contract`, "GPU program").
//!
//! The core runs no GPU and no WebAssembly. The engine crate keeps a program's state and runs the
//! probe; the desktop validates the program and draws it. What they share lives here as data:
//!
//! - [`ProgramState`], what `module.status` reports of a module's program.
//! - [`ModuleManifest::probe_values`], the value sets of the manifest's table a probe and `cargo
//!   xtask module-check` run: the defaults, every field at its least, middle and greatest, each
//!   field at each end of its range with the rest at their defaults, and three mixed sets.
//! - [`ProbeDifference`], how far the program's pixels are from the reference's over one probe
//!   patch, as 8-bit sRGB through CIEDE2000, and [`COLOUR_LIMITS`], the colour (pointwise) class's
//!   preview error limit it must be within (`docs/design/gpu-first.md`, "Tolerance by output
//!   kind").
use super::{FieldEntry, FieldValue, ModuleManifest};
use crate::Error;
use crate::colour::{cielab, srgb};
use crate::modules::{ParameterDescriptor, ParameterKind};
use serde_json::{Value, json};

/// What became of an external module's `program.wgsl`, as `module.status` and
/// `module.package.list` report it, serialized as [`Self::to_json`] answers it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum ProgramState {
    /// The package carries no program: the module renders through the reference.
    None,
    /// Neither validated nor probed, and rendered through the reference: this host draws nothing
    /// on a GPU (`luxforge-json`), or, in a status row, this launch did not load the package
    /// (stored since it started, or it failed to load).
    Unchecked,
    /// The program did not validate under the surface's prelude, or declares a name it may not:
    /// it is not registered, and the module renders through the reference.
    Refused { reason: String },
    /// Validated and registered, and disabled until its first use probes it.
    Pending,
    /// Its probe is running on a worker.
    Probing,
    /// Its probe held it within the colour class's limits on `adapter`: plans draw it.
    Enabled { adapter: String },
    /// Its probe found it outside those limits on `adapter`, or could not run it, for `reason`: the
    /// module renders through the reference, the plan naming the program
    /// ([`crate::GpuFallback::DisabledProgram`]).
    Disabled { adapter: String, reason: String },
}

impl ProgramState {
    /// A stable kebab-case name for the state.
    pub fn code(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Unchecked => "unchecked",
            Self::Refused { .. } => "refused",
            Self::Pending => "pending",
            Self::Probing => "probing",
            Self::Enabled { .. } => "enabled",
            Self::Disabled { .. } => "disabled",
        }
    }

    /// `{"state", "adapter"?, "reason"?}`, as a status answer carries it.
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or_else(|_| json!({ "state": self.code() }))
    }
}

/// One named value set of a manifest's table: every field, in the manifest's order.
#[derive(Clone, Debug, PartialEq)]
pub struct ProbeValues {
    pub name: String,
    pub values: Vec<FieldEntry>,
}

impl ModuleManifest {
    /// The fields of the table, in the manifest's order: the parameters of its `set-<name>`
    /// action.
    pub fn fields(&self) -> &[ParameterDescriptor] {
        let set = format!("set-{}", self.name);
        self.descriptor()
            .actions
            .iter()
            .find(|action| action.id == set)
            .map_or(&[], |action| &action.parameters)
    }

    /// The value sets a probe of the module's program runs, and `module-check` with it: the
    /// defaults; every field at its least, middle and greatest; each field at each end of its
    /// range with the rest at their defaults; and three mixed sets from a fixed sequence. At most
    /// `7 + 2 × 64` sets, since a table holds at most 64 fields.
    pub fn probe_values(&self) -> Result<Vec<ProbeValues>, Error> {
        probe_values(self.fields())
    }
}

/// [`ModuleManifest::probe_values`] over `parameters`.
pub fn probe_values(parameters: &[ParameterDescriptor]) -> Result<Vec<ProbeValues>, Error> {
    let defaults: Vec<FieldEntry> = parameters
        .iter()
        .map(|parameter| {
            Ok(FieldEntry {
                name: parameter.name.clone(),
                value: default_of(parameter)?,
            })
        })
        .collect::<Result<_, Error>>()?;
    let field = |parameter: &ParameterDescriptor, t: f64| FieldEntry {
        name: parameter.name.clone(),
        value: at(parameter, t),
    };
    let uniform = |t: f64| -> Vec<FieldEntry> {
        parameters
            .iter()
            .map(|parameter| field(parameter, t))
            .collect()
    };
    let named = |name: &str, values: Vec<FieldEntry>| ProbeValues {
        name: name.to_owned(),
        values,
    };
    let mut sets = vec![
        named("defaults", defaults.clone()),
        named("minimum", uniform(0.0)),
        named("middle", uniform(0.5)),
        named("maximum", uniform(1.0)),
    ];
    for (index, parameter) in parameters.iter().enumerate() {
        for (end, t) in [("min", 0.0), ("max", 1.0)] {
            let mut values = defaults.clone();
            values[index] = field(parameter, t);
            sets.push(named(&format!("{}={end}", parameter.name), values));
        }
    }
    let mut state = 0x2545_f491_u32;
    for mixed in 1..=3 {
        let values = parameters
            .iter()
            .map(|parameter| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                field(parameter, f64::from(state >> 8) / 16_777_215.0)
            })
            .collect();
        sets.push(named(&format!("mixed-{mixed}"), values));
    }
    Ok(sets)
}

fn default_of(parameter: &ParameterDescriptor) -> Result<FieldValue, Error> {
    let name = &parameter.name;
    let default = parameter
        .default
        .as_ref()
        .ok_or_else(|| Error::internal(format!("field {name} has no default")))?;
    let mismatch = || Error::internal(format!("field {name}'s default {default} is not its kind"));
    Ok(match &parameter.kind {
        ParameterKind::Number { .. } => FieldValue::Number(default.as_f64().ok_or_else(mismatch)?),
        ParameterKind::Integer { .. } => {
            FieldValue::Integer(default.as_i64().ok_or_else(mismatch)?)
        }
        ParameterKind::Boolean => FieldValue::Boolean(default.as_bool().ok_or_else(mismatch)?),
        ParameterKind::Enum { .. } => {
            FieldValue::Choice(default.as_str().ok_or_else(mismatch)?.to_owned())
        }
        ParameterKind::Color => {
            let channel = |at: usize| {
                default
                    .get(at)
                    .and_then(Value::as_u64)
                    .and_then(|channel| u8::try_from(channel).ok())
                    .ok_or_else(mismatch)
            };
            FieldValue::Color([channel(0)?, channel(1)?, channel(2)?])
        }
        _ => {
            return Err(Error::internal(format!(
                "field {name} is of a kind a manifest has not"
            )));
        }
    })
}

/// A field's value at `t` of its range, 0 its least and 1 its greatest: a number or integer within
/// its range (a number to two decimals, as a person types one), a boolean false below the middle,
/// an enum's option at that share of its list, and a grey of that level.
fn at(parameter: &ParameterDescriptor, t: f64) -> FieldValue {
    match &parameter.kind {
        ParameterKind::Number { min, max } => {
            FieldValue::Number(((min + t * (max - min)) * 100.0).round() / 100.0)
        }
        ParameterKind::Integer { min, max } => {
            FieldValue::Integer(*min + ((*max - *min) as f64 * t).round() as i64)
        }
        ParameterKind::Boolean => FieldValue::Boolean(t >= 0.5),
        ParameterKind::Enum { options } => {
            let last = options.len().saturating_sub(1);
            FieldValue::Choice(options[(last as f64 * t).round() as usize].clone())
        }
        _ => FieldValue::Color([(t * 255.0).round() as u8; 3]),
    }
}

/// One class's four preview error limits: mean ΔE00, the worst 16 × 16 block's mean ΔE00, the
/// nearest-rank 99th percentile ΔE00 and the absolute signed mean ΔL\*, each at most.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProbeLimits {
    pub mean: f64,
    pub worst_block: f64,
    pub p99: f64,
    pub mean_delta_l: f64,
}

/// The colour (pointwise) class's limits, which a probe holds an external program to: the
/// preview error limit of `docs/design/gpu-preview.md`, the same as the release gate's.
pub const COLOUR_LIMITS: ProbeLimits = ProbeLimits {
    mean: 0.5,
    worst_block: 1.0,
    p99: 2.0,
    mean_delta_l: 0.25,
};

/// The side of a block of the worst-block statistic, in pixels.
const BLOCK: usize = 16;

/// How far one patch of a program's pixels is from the reference's: each pixel quantized to 8-bit
/// sRGB by the output quantizer, taken through linear light and CIELAB, and compared with
/// CIEDE2000, the measure `luxforge_reference::preview_error` states independently.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ProbeDifference {
    pub mean: f64,
    pub worst_block: f64,
    pub p99: f64,
    /// Signed: the program's lightness less the reference's.
    pub mean_delta_l: f64,
}

impl ProbeDifference {
    /// `candidate` (the program's) against `reference`, both `width × height` linear-sRGB pixels
    /// row by row, or why they cannot be compared: different or empty sizes, or a pixel the
    /// candidate left not finite where the reference's is finite, which no limit admits.
    pub fn of(
        candidate: &[[f32; 3]],
        reference: &[[f32; 3]],
        width: usize,
        height: usize,
    ) -> Result<Self, String> {
        let count = width * height;
        if count == 0 || candidate.len() != count || reference.len() != count {
            return Err(format!(
                "{} and {} pixels for a patch of {width} × {height}",
                candidate.len(),
                reference.len()
            ));
        }
        if let Some(at) = (0..count).find(|&at| {
            reference[at].iter().all(|v| v.is_finite())
                && !candidate[at].iter().all(|v| v.is_finite())
        }) {
            return Err(format!(
                "pixel ({}, {}) is {:?}, not finite, where the reference's is {:?}",
                at % width,
                at / width,
                candidate[at],
                reference[at]
            ));
        }
        let lab = |code: [u8; 3]| cielab::from_linear(code.map(srgb::decode_u8));
        let mut delta_e = Vec::with_capacity(count);
        let mut delta_l = 0.0;
        for (c, r) in candidate.iter().zip(reference) {
            let (c, r) = (srgb::quantize_pixel(*c), srgb::quantize_pixel(*r));
            if c == r {
                delta_e.push(0.0);
            } else {
                let (lab_c, lab_r) = (lab(c), lab(r));
                delta_e.push(cielab::ciede2000(lab_r, lab_c));
                delta_l += lab_c[0] - lab_r[0];
            }
        }
        let mean = delta_e.iter().sum::<f64>() / count as f64;
        let starts = |side: usize| -> Vec<usize> {
            if side <= BLOCK {
                return vec![0];
            }
            let mut starts: Vec<usize> = (0..side - BLOCK).step_by(BLOCK).collect();
            starts.push(side - BLOCK);
            starts
        };
        let (block_w, block_h) = (width.min(BLOCK), height.min(BLOCK));
        let mut worst_block = 0.0_f64;
        for &y0 in &starts(height) {
            for &x0 in &starts(width) {
                let sum: f64 = (y0..y0 + block_h)
                    .flat_map(|y| (x0..x0 + block_w).map(move |x| y * width + x))
                    .map(|at| delta_e[at])
                    .sum();
                worst_block = worst_block.max(sum / (block_w * block_h) as f64);
            }
        }
        // The nearest rank of the 99th percentile: the largest value left once the worst
        // `count / 100` are set aside.
        let rank = count - 1 - count / 100;
        let (_, p99, _) = delta_e.select_nth_unstable_by(rank, f64::total_cmp);
        Ok(Self {
            mean,
            worst_block,
            p99: *p99,
            mean_delta_l: delta_l / count as f64,
        })
    }

    /// Whether every statistic is within `limits`, inclusively; the signed mean ΔL\* by its size.
    pub fn within(&self, limits: &ProbeLimits) -> bool {
        self.mean <= limits.mean
            && self.worst_block <= limits.worst_block
            && self.p99 <= limits.p99
            && self.mean_delta_l.abs() <= limits.mean_delta_l
    }

    /// The largest of each statistic of `self` and `other`, the signed mean ΔL\* by its size.
    pub fn max(self, other: Self) -> Self {
        Self {
            mean: self.mean.max(other.mean),
            worst_block: self.worst_block.max(other.worst_block),
            p99: self.p99.max(other.p99),
            mean_delta_l: if other.mean_delta_l.abs() > self.mean_delta_l.abs() {
                other.mean_delta_l
            } else {
                self.mean_delta_l
            },
        }
    }
}

impl std::fmt::Display for ProbeDifference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "mean {:.3}, worst block {:.3}, p99 {:.3}, mean ΔL* {:+.3}",
            self.mean, self.worst_block, self.p99, self.mean_delta_l
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use luxforge_reference::preview_error::{self, Class, Rgb8};

    #[test]
    fn the_colour_limits_are_the_pointwise_class_limits() {
        let limits = Class::Pointwise.limits();
        assert_eq!(
            COLOUR_LIMITS,
            ProbeLimits {
                mean: limits.mean,
                worst_block: limits.worst_block,
                p99: limits.p99,
                mean_delta_l: limits.mean_delta_l,
            }
        );
    }

    /// A patch of `width × height` from a fixed sequence, and the same patch moved by up to
    /// `spread` in linear light.
    fn pair(width: usize, height: usize, spread: f32) -> (Vec<[f32; 3]>, Vec<[f32; 3]>) {
        let mut state = 0x9e37_79b9_u32;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state >> 8) as f32 / 16_777_216.0
        };
        let reference: Vec<[f32; 3]> = (0..width * height)
            .map(|_| [next(), next(), next()].map(|v| v * 1.2 - 0.1))
            .collect();
        let candidate = reference
            .iter()
            .map(|pixel| pixel.map(|v| v + (next() - 0.4) * spread))
            .collect();
        (candidate, reference)
    }

    fn codes(pixels: &[[f32; 3]]) -> Vec<u8> {
        pixels
            .iter()
            .flat_map(|pixel| srgb::quantize_pixel(*pixel))
            .collect()
    }

    /// The probe's statistics are the independent measure's over the same codes, at sizes whose
    /// blocks tile exactly, overlap at an edge, and span a side shorter than a block.
    #[test]
    fn the_probe_difference_is_the_reference_measure() {
        for (width, height, spread) in [(64, 64, 0.02), (37, 23, 0.05), (9, 40, 0.01)] {
            let (candidate, reference) = pair(width, height, spread);
            let ours = ProbeDifference::of(&candidate, &reference, width, height).expect("sizes");
            let (c, r) = (codes(&candidate), codes(&reference));
            let theirs = preview_error::compare(
                Rgb8::new(width as u32, height as u32, &c).expect("a frame"),
                Rgb8::new(width as u32, height as u32, &r).expect("a frame"),
                [0, 0, width as u32, height as u32],
            )
            .expect("comparable");
            let close = |a: f64, b: f64| (a - b).abs() <= 1e-9 * (1.0 + b.abs());
            assert!(close(ours.mean, theirs.mean), "{ours:?} {theirs:?}");
            assert!(
                close(ours.worst_block, theirs.worst_block),
                "{ours:?} {theirs:?}"
            );
            assert!(close(ours.p99, theirs.p99), "{ours:?} {theirs:?}");
            assert!(
                close(ours.mean_delta_l, theirs.mean_delta_l),
                "{ours:?} {theirs:?}"
            );
            assert_eq!(
                ours.within(&COLOUR_LIMITS),
                preview_error::verdict(&theirs, Class::Pointwise).passed()
            );
        }
    }

    #[test]
    fn a_pixel_left_not_finite_is_no_difference_but_a_refusal() {
        let (mut candidate, reference) = pair(16, 16, 0.0);
        assert!(
            ProbeDifference::of(&candidate, &reference, 16, 16)
                .expect("equal")
                .within(&COLOUR_LIMITS)
        );
        candidate[17][1] = f32::NAN;
        let refused = ProbeDifference::of(&candidate, &reference, 16, 16).unwrap_err();
        assert!(refused.starts_with("pixel (1, 1) is"), "{refused}");
    }

    #[test]
    fn program_states_report_their_code_and_details() {
        assert_eq!(ProgramState::Pending.to_json(), json!({"state": "pending"}));
        assert_eq!(ProgramState::None.to_json(), json!({"state": "none"}));
        assert_eq!(
            ProgramState::Enabled {
                adapter: "A".into()
            }
            .to_json(),
            json!({"state": "enabled", "adapter": "A"})
        );
        assert_eq!(
            ProgramState::Disabled {
                adapter: "A".into(),
                reason: "why".into()
            }
            .to_json(),
            json!({"state": "disabled", "adapter": "A", "reason": "why"})
        );
    }
}
