//! An external module's GPU program (`docs/design/module-manager.md#contract`, "GPU program"):
//! registered when its package loads, disabled until a probe at first use enables it, and the
//! probe's result cached per package hash and adapter.
//!
//! - **Registering.** A host that draws on a GPU hands [`WasmHost`](crate::WasmHost) its
//!   [`Programs`], whose [`ProgramLoader`] checks `program.wgsl` under the surface's prelude and
//!   gives its names the package's unique entry, `lf_ext_` and the first 16 hex digits of the
//!   package hash ([`entry_of`]). A program it refuses is not registered: [`ProgramState::Refused`]
//!   with the reason, and the module's units have no GPU description, so a plan falls back naming
//!   the unit, whose description says its program was refused. A host without [`Programs`]
//!   (`luxforge-json`, which draws nothing) leaves the program [`ProgramState::Unchecked`].
//! - **The launch's programs.** A registered program's text and the core's
//!   [`GpuProgram`](luxforge_core::GpuProgram) describing it are made `'static` once per load, as
//!   the built-in programs are: a launch loads each installed package once, so this is bounded by
//!   the packages installed (a program is at most 256 KiB). The core's plans then carry it as they
//!   carry a built-in program, and the surface and tile worker compile it with the rest.
//! - **Disabled until probed.** A unit's description names the program disabled until its probe
//!   enables it, so a plan answers [`GpuFallback::DisabledProgram`](luxforge_core::GpuFallback)
//!   naming its entry and the stack renders through the reference.
//! - **The probe** ([`Probe`], [`ProbeRun`]) is asked for by the host where a plan fell back on a
//!   pending program, and runs on the host's GPU worker, never at launch: for each value set of the
//!   manifest's table ([`ModuleManifest::probe_values`]) it takes the component's words, runs the
//!   reference (`apply-row`) over the fixed probe patches, has the host run the program over the
//!   same patches, and holds every patch to the colour class's limits ([`COLOUR_LIMITS`]). A step
//!   is one value set, so the worker can stop between steps; a run dropped before its end leaves
//!   the program pending, for the next use to probe again. Within the limits on every patch, the
//!   program is enabled with the word count every set answered; otherwise, or when the words'
//!   count varies or the reference or the GPU fails, it stays disabled with the reason.
//! - **Cached per package hash and adapter**, under the data root ([`Programs::new`]'s
//!   directory): one small JSON file a pair, written to a temporary name and renamed into place,
//!   so a later launch on the same adapter applies the result without probing. A torn or foreign
//!   entry is ignored and the program probed again.
use crate::Adjustment;
use luxforge_core::module_package::{
    COLOUR_LIMITS, ModuleManifest, ProbeDifference, ProbeValues, ProgramState,
};
use luxforge_core::{GpuProgram, GpuProgramKind};
use luxforge_gpu_types::probe::{Patch, patches};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, Weak};

/// The entry every package's program declares before the loader renames it.
pub const PROGRAM_ENTRY: &str = "lf_module";

/// What changes a cached probe result's meaning: the patches, the value sets or the limits. A
/// cache entry of another format is ignored.
const PROBE_FORMAT: u64 = 1;

/// The largest cache entry read back.
const MAX_ENTRY_BYTES: u64 = 64 * 1024;

/// Checks a package's `program.wgsl` and renames it, for a host that draws on a GPU: the desktop's
/// is `luxforge_gpu::external_program::register`.
pub trait ProgramLoader: Send + Sync {
    /// `source` checked under the surface's prelude with every name it declares given `entry` in
    /// place of [`PROGRAM_ENTRY`]: the registered text, or why it is refused.
    fn register(&self, source: &str, entry: &str) -> Result<String, String>;
}

/// The entry a package's program is registered under: `lf_ext_` and the first 16 hex digits of its
/// package hash, so two packages' programs never share a name and the same package's always do.
pub fn entry_of(package: &str) -> String {
    let digest = match package.strip_prefix("sha256:") {
        Some(hex) if hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()) => {
            hex.to_ascii_lowercase()
        }
        _ => format!("{:x}", Sha256::digest(package.as_bytes())),
    };
    format!("lf_ext_{}", &digest[..16])
}

/// A launch's external programs: the loader that registers them, where probe results are cached,
/// and every registered program by its entry, for the host to probe the one a plan names.
pub struct Programs {
    loader: Arc<dyn ProgramLoader>,
    cache: Option<PathBuf>,
    by_entry: Mutex<HashMap<&'static str, Weak<Adjustment>>>,
}

impl std::fmt::Debug for Programs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Programs")
            .field("cache", &self.cache)
            .finish_non_exhaustive()
    }
}

impl Programs {
    /// Programs registered by `loader`, their probe results cached in `cache` (created when first
    /// written), or probed at every launch with none.
    pub fn new(loader: Arc<dyn ProgramLoader>, cache: Option<PathBuf>) -> Arc<Self> {
        Arc::new(Self {
            loader,
            cache,
            by_entry: Mutex::new(HashMap::new()),
        })
    }

    /// The probe of the registered program `entry`, the name a plan's
    /// [`GpuFallback::DisabledProgram`](luxforge_core::GpuFallback) gives; `None` for a name that
    /// is not an external program of this launch.
    pub fn probe(&self, entry: &str) -> Option<Probe> {
        let adjustment = self
            .by_entry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(entry)?
            .upgrade()?;
        adjustment.registered()?;
        Some(Probe { adjustment })
    }

    /// `adjustment`'s program, registered from `source`.
    pub(crate) fn register(
        self: &Arc<Self>,
        package: &str,
        manifest: &ModuleManifest,
        source: &str,
    ) -> Registration {
        let entry = entry_of(package);
        let values = match manifest.probe_values() {
            Ok(values) => values,
            Err(error) => return Registration::Refused(error.detail),
        };
        match self.loader.register(source, &entry) {
            Ok(text) => Registration::Registered(Arc::new(Program::new(
                package, entry, text, values, self,
            ))),
            Err(reason) => Registration::Refused(reason),
        }
    }

    /// Record `adjustment`'s registered program by its entry.
    pub(crate) fn add(&self, adjustment: &Arc<Adjustment>) {
        if let Some(program) = adjustment.registered() {
            self.by_entry
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(program.entry, Arc::downgrade(adjustment));
        }
    }
}

/// What a loaded module's program is.
pub(crate) enum Registration {
    /// The package carries none.
    None,
    /// This host checks no program.
    Unchecked,
    /// The loader refused it, for this reason.
    Refused(String),
    Registered(Arc<Program>),
}

impl Registration {
    pub(crate) fn state(&self) -> ProgramState {
        match self {
            Self::None => ProgramState::None,
            Self::Unchecked => ProgramState::Unchecked,
            Self::Refused(reason) => ProgramState::Refused {
                reason: reason.clone(),
            },
            Self::Registered(program) => program.state(),
        }
    }
}

/// Where a registered program's probe stands.
#[derive(Clone)]
enum Stage {
    Pending,
    Probing,
    Enabled {
        adapter: String,
        program: &'static GpuProgram,
    },
    Disabled {
        adapter: String,
        reason: String,
    },
}

/// A registered program: its `'static` text and the core's descriptions of it.
pub(crate) struct Program {
    package: String,
    pub(crate) entry: &'static str,
    source: &'static str,
    values: Vec<ProbeValues>,
    stage: Mutex<Stage>,
    /// The disabled description for each word count a unit has asked with: at most one per count
    /// a unit holds, so bounded by [`crate::MAX_WORDS`].
    disabled: Mutex<HashMap<usize, &'static GpuProgram>>,
    cache: Option<PathBuf>,
}

impl Program {
    fn new(
        package: &str,
        entry: String,
        source: String,
        values: Vec<ProbeValues>,
        programs: &Programs,
    ) -> Self {
        Self {
            package: package.to_owned(),
            entry: Box::leak(entry.into_boxed_str()),
            source: Box::leak(source.into_boxed_str()),
            values,
            stage: Mutex::new(Stage::Pending),
            disabled: Mutex::new(HashMap::new()),
            cache: programs.cache.clone(),
        }
    }

    fn stage(&self) -> std::sync::MutexGuard<'_, Stage> {
        self.stage.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn state(&self) -> ProgramState {
        match &*self.stage() {
            Stage::Pending => ProgramState::Pending,
            Stage::Probing => ProgramState::Probing,
            Stage::Enabled { adapter, .. } => ProgramState::Enabled {
                adapter: adapter.clone(),
            },
            Stage::Disabled { adapter, reason } => ProgramState::Disabled {
                adapter: adapter.clone(),
                reason: reason.clone(),
            },
        }
    }

    /// The core's program for a unit of `words` words: the enabled one when the probe enabled the
    /// program for that count, else a disabled one, which a plan falls back on by name.
    pub(crate) fn described(&self, words: usize) -> &'static GpuProgram {
        if let Stage::Enabled { program, .. } = &*self.stage()
            && program.words == words
        {
            return program;
        }
        *self
            .disabled
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(words)
            .or_insert_with(|| self.leak(words, false))
    }

    fn leak(&self, words: usize, enabled: bool) -> &'static GpuProgram {
        Box::leak(Box::new(GpuProgram {
            entry: self.entry,
            source: self.source,
            kind: GpuProgramKind::Colour,
            words,
            enabled,
        }))
    }

    /// The program enabled for `words` words on `adapter`, or disabled there for `reason`.
    fn settle(&self, adapter: &str, outcome: Result<usize, String>) {
        let stage = match outcome {
            Ok(words) => Stage::Enabled {
                adapter: adapter.to_owned(),
                program: self.leak(words, true),
            },
            Err(reason) => Stage::Disabled {
                adapter: adapter.to_owned(),
                reason,
            },
        };
        *self.stage() = stage;
    }

    /// This program's cache entry on `adapter`.
    fn entry_path(&self, adapter: &str) -> Option<PathBuf> {
        let digest = Sha256::digest(format!("{}\n{adapter}\n{PROBE_FORMAT}", self.package));
        let package = entry_of(&self.package);
        let name = format!(
            "{}-{}.json",
            package.trim_start_matches("lf_ext_"),
            digest[..8]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        Some(self.cache.as_ref()?.join(name))
    }

    /// The cached result on `adapter`: words when enabled, the reason when disabled; `None` when
    /// there is no valid entry for this package, adapter and probe format.
    fn cached(&self, adapter: &str) -> Option<Result<usize, String>> {
        use std::io::Read;
        let path = self.entry_path(adapter)?;
        let mut text = String::new();
        std::fs::File::open(path)
            .ok()?
            .take(MAX_ENTRY_BYTES)
            .read_to_string(&mut text)
            .ok()?;
        let entry: Value = serde_json::from_str(&text).ok()?;
        if entry["format"].as_u64() != Some(PROBE_FORMAT)
            || entry["package"].as_str() != Some(&self.package)
            || entry["adapter"].as_str() != Some(adapter)
        {
            return None;
        }
        match entry["enabled"].as_bool()? {
            true => Some(Ok(usize::try_from(entry["words"].as_u64()?).ok()?)),
            false => Some(Err(entry["reason"].as_str()?.to_owned())),
        }
    }

    /// Record `outcome` on `adapter`; a cache that cannot be written costs a probe next launch.
    fn store(&self, adapter: &str, outcome: &Result<usize, String>) {
        let Some(path) = self.entry_path(adapter) else {
            return;
        };
        let entry = match outcome {
            Ok(words) => json!({"enabled": true, "words": words}),
            Err(reason) => json!({"enabled": false, "reason": reason}),
        };
        let mut entry = entry;
        entry["format"] = json!(PROBE_FORMAT);
        entry["package"] = json!(self.package);
        entry["adapter"] = json!(adapter);
        let _ = write(&path, entry.to_string().as_bytes());
    }
}

/// Write `bytes` to `path` through a temporary name beside it, renamed into place, so a reader
/// sees a whole entry or none. Not flushed to the drive: a torn entry does not parse and is
/// probed again.
fn write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
    let temporary = crate::cache::temporary(path);
    let written =
        std::fs::write(&temporary, bytes).and_then(|()| std::fs::rename(&temporary, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

/// A registered program and its module, for the host to probe.
#[derive(Clone)]
pub struct Probe {
    adjustment: Arc<Adjustment>,
}

impl std::fmt::Debug for Probe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Probe")
            .field("module", &self.adjustment.id())
            .field("entry", &self.entry())
            .finish()
    }
}

impl Probe {
    fn program(&self) -> &Program {
        self.adjustment
            .registered()
            .expect("a probe is made only of a registered program")
    }

    /// The module the program belongs to.
    pub fn module(&self) -> &str {
        self.adjustment.id()
    }

    /// The program's entry, as plans name it.
    pub fn entry(&self) -> &'static str {
        self.program().entry
    }

    /// The program's registered text, functions and constants only.
    pub fn source(&self) -> &'static str {
        self.program().source
    }

    /// Where the program stands now.
    pub fn state(&self) -> ProgramState {
        self.program().state()
    }

    /// A run of the probe on `adapter` (the host's stable description of it: name, backend and
    /// driver), or `None` when none is needed: the program is enabled or disabled on that adapter
    /// already, a probe is running, or the cache held the result for this package and adapter,
    /// which is then applied.
    pub fn begin(&self, adapter: &str) -> Option<ProbeRun> {
        let program = self.program();
        {
            let mut stage = program.stage();
            match &*stage {
                Stage::Probing => return None,
                Stage::Enabled { adapter: on, .. } | Stage::Disabled { adapter: on, .. }
                    if on == adapter =>
                {
                    return None;
                }
                _ => {}
            }
            *stage = Stage::Probing;
        }
        if let Some(outcome) = program.cached(adapter) {
            program.settle(adapter, outcome);
            return None;
        }
        Some(ProbeRun {
            probe: self.clone(),
            adapter: adapter.to_owned(),
            patches: patches(),
            next: 0,
            words: None,
            worst: ProbeDifference::default(),
            ended: false,
        })
    }
}

/// One probe of a registered program on one adapter, a value set a step. Dropped before it ends,
/// it leaves the program pending.
pub struct ProbeRun {
    probe: Probe,
    adapter: String,
    patches: Vec<Patch>,
    next: usize,
    /// The word count every value set so far answered.
    words: Option<usize>,
    /// The largest difference so far.
    worst: ProbeDifference,
    ended: bool,
}

impl std::fmt::Debug for ProbeRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProbeRun")
            .field("probe", &self.probe)
            .field("adapter", &self.adapter)
            .field("next", &self.next)
            .finish()
    }
}

/// What the host's side of a step answers: the program's pixels over every patch, in the patches'
/// order, for the step's words.
pub type ProgramPixels = Result<Vec<Vec<[f32; 3]>>, String>;

impl ProbeRun {
    /// The probe this run is of.
    pub fn probe(&self) -> &Probe {
        &self.probe
    }

    /// The patches the host runs the program over, in order.
    pub fn patches(&self) -> &[Patch] {
        &self.patches
    }

    /// How many value sets the run takes, one a step.
    pub fn steps(&self) -> usize {
        self.probe.program().values.len()
    }

    /// The next value set: the component's words for it, the reference over every patch, and the
    /// program's pixels from `gpu`, held to the colour class's limits. `true` while more remain;
    /// `false` once the run has ended, the program enabled or disabled and the result cached.
    pub fn step(&mut self, gpu: &mut dyn FnMut(&[u32]) -> ProgramPixels) -> bool {
        if self.ended {
            return false;
        }
        let probe = self.probe.clone();
        let program = probe.program();
        let Some(set) = program.values.get(self.next) else {
            let words = self.words.unwrap_or(0);
            self.end(Ok(words));
            return false;
        };
        match self.judge(set, gpu) {
            Ok(()) => {
                self.next += 1;
                if self.next == program.values.len() {
                    let words = self.words.unwrap_or(0);
                    self.end(Ok(words));
                    return false;
                }
                true
            }
            Err(reason) => {
                self.end(Err(format!("value set `{}`: {reason}", set.name)));
                false
            }
        }
    }

    /// End the run with the program disabled for `reason`, a failure of the host's side that is
    /// not the program's difference (the device refused its pipeline, or was lost).
    pub fn fail(mut self, reason: String) {
        self.end(Err(reason));
    }

    fn judge(
        &mut self,
        set: &ProbeValues,
        gpu: &mut dyn FnMut(&[u32]) -> ProgramPixels,
    ) -> Result<(), String> {
        let adjustment = &self.probe.adjustment;
        let words = adjustment
            .coefficients(&set.values)
            .map_err(|error| error.detail)?;
        match self.words {
            Some(count) if count != words.len() => {
                return Err(format!(
                    "the component answered {} words here and {count} before, and a GPU program \
                     reads a fixed count",
                    words.len()
                ));
            }
            _ => self.words = Some(words.len()),
        }
        let drawn = gpu(&words)?;
        if drawn.len() != self.patches.len() {
            return Err(format!(
                "the GPU answered {} patches of {}",
                drawn.len(),
                self.patches.len()
            ));
        }
        for (patch, drawn) in self.patches.iter().zip(&drawn) {
            let mut reference = patch.rgb.clone();
            for (y, row) in reference.chunks_mut(patch.width as usize).enumerate() {
                adjustment
                    .apply_row(&words, y as u32, 0, row)
                    .map_err(|error| format!("patch `{}`: {}", patch.name, error.detail))?;
            }
            let difference = ProbeDifference::of(
                drawn,
                &reference,
                patch.width as usize,
                patch.height as usize,
            )
            .map_err(|error| format!("patch `{}`: {error}", patch.name))?;
            self.worst = self.worst.max(difference);
            if !difference.within(&COLOUR_LIMITS) {
                return Err(format!(
                    "patch `{}`: {difference}, outside the colour class's limits",
                    patch.name
                ));
            }
        }
        Ok(())
    }

    fn end(&mut self, outcome: Result<usize, String>) {
        self.ended = true;
        let program = self.probe.program();
        program.store(&self.adapter, &outcome);
        program.settle(&self.adapter, outcome);
    }
}

impl Drop for ProbeRun {
    /// A run that did not end leaves the program pending, for its next use to probe again.
    fn drop(&mut self) {
        if !self.ended {
            *self.probe.program().stage() = Stage::Pending;
        }
    }
}
