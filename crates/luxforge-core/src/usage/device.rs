//! Gathering the device profile from facts the app already has: the platform probe
//! (`luxforge_process::system_info`), the normalized facts of the adapters the app selected
//! (`luxforge_gpu_types::adapter_facts`) and the renderer path. Nothing here opens a device,
//! enumerates adapters or reads hardware identifiers; it only combines normalized values into the
//! validated [`DeviceProfile`] and holds the result for one consent generation.
//!
//! - **Once per opted-in launch, on a worker.** [`DeviceContext::update`] probes the platform the
//!   first time it runs in a consent generation (a few file and system calls; never the UI or
//!   owner thread) and afterwards only recombines the cached platform values with whatever
//!   adapters and renderer path the caller reports. The caller calls it again only when the
//!   existing renderer machinery reports an adapter or path change; there is no polling loop.
//! - **Consent first.** Nothing is probed without a current [`Generation`] on the gate, a probe
//!   that overlaps a revocation is discarded, and the held profile is invisible
//!   ([`DeviceContext::profile`]) from the moment the gate leaves the generation it was gathered in.
//! - **No raw strings.** Every input value is already an enum, a bounded integer or numeric version
//!   components, so a value the probes did not recognise is simply absent or `other`.
//! - **When a profile cannot be formed** ([`GatherError::Missing`], [`GatherError::Invalid`]): the
//!   wire requires an OS version, both core counts and the installed-memory band, and this module
//!   will not invent them. The supervisor treats it as "no reportable device for this launch":
//!   it stops the collector for the generation (so nothing is buffered that could not be sent),
//!   records the fixed reason in local diagnostics only, and does not retry until the next
//!   launch or opt-in. An adapter change cannot cause this: the platform part is cached.
use super::{
    device_profile::{
        Arch, CpuProfile, DeviceProfile, GpuProfile, MemoryBand, OsProfile, ProfileError,
        RendererFallback,
    },
    gate::{Gate, Generation},
    registry::Renderer,
};
/// The normalized facts of one adapter, as `luxforge_gpu::adapters::Adapter::facts` returns them.
pub use luxforge_gpu_types::adapter_facts::GpuFacts;
use luxforge_process::system_info::{self as sys, SystemFacts};

/// The renderer path that draws the desktop's picture, with why the reference does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RendererPath {
    /// The GPU stage draws, on a hardware or software adapter.
    Gpu,
    /// The reference renderer draws; the reason is `None` only where there is no GPU provider to
    /// refuse (a headless owner).
    Reference(Option<RendererFallback>),
}

impl RendererPath {
    fn parts(self) -> (Renderer, Option<RendererFallback>) {
        match self {
            Self::Gpu => (Renderer::Gpu, None),
            Self::Reference(fallback) => (Renderer::Reference, fallback),
        }
    }
}

/// Why no device profile was formed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GatherError {
    /// Collection is off or this is not the current consent generation: nothing was probed, or
    /// what was probed was discarded.
    NotConsented,
    /// The platform did not give a value the wire requires.
    Missing(&'static str),
    /// The values do not make a valid profile (see [`ProfileError`]).
    Invalid(ProfileError),
}

impl std::fmt::Display for GatherError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConsented => f.write_str("statistics are not enabled"),
            Self::Missing(what) => write!(f, "the platform did not report {what}"),
            Self::Invalid(error) => write!(f, "no valid device profile: {error}"),
        }
    }
}

impl std::error::Error for GatherError {}

impl From<ProfileError> for GatherError {
    fn from(error: ProfileError) -> Self {
        Self::Invalid(error)
    }
}

/// The platform half of a profile: operating system, architecture, CPU and installed memory.
/// None of it changes while the process runs, so it is gathered once per consent generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlatformProfile {
    os: OsProfile,
    arch: Arch,
    cpu: CpuProfile,
    memory: MemoryBand,
}

impl PlatformProfile {
    /// The probed facts as profile values, or what is missing.
    pub fn from_facts(facts: &SystemFacts) -> Result<Self, GatherError> {
        Ok(Self {
            os: OsProfile::from_facts(&facts.os).ok_or(GatherError::Missing("the OS version"))?,
            arch: facts.arch.into(),
            cpu: CpuProfile::from_facts(&facts.cpu)
                .ok_or(GatherError::Missing("the CPU core counts"))?,
            memory: facts
                .memory
                .ok_or(GatherError::Missing("the installed memory"))?
                .into(),
        })
    }

    /// The whole profile for the adapters and renderer path reported now.
    pub fn profile(
        &self,
        gpus: &[GpuFacts],
        path: RendererPath,
    ) -> Result<DeviceProfile, GatherError> {
        let gpus = gpu_profiles(gpus);
        let (renderer, fallback) = path.parts();
        Ok(DeviceProfile::new(
            self.os,
            self.arch,
            self.cpu,
            self.memory,
            &gpus,
            renderer,
            fallback,
        )?)
    }
}

/// The most GPUs a profile lists.
pub const MAX_GPUS: usize = 2;

/// The listed adapters as profiles: in the order given (the tile worker's adapter, then the
/// window's), each model once, at most [`MAX_GPUS`]. The dedicated-memory band is left out: no
/// existing adapter report gives one, and nothing is queried to find it.
pub fn gpu_profiles(facts: &[GpuFacts]) -> Vec<GpuProfile> {
    let mut listed: Vec<GpuProfile> = Vec::with_capacity(MAX_GPUS);
    for facts in facts {
        let gpu = GpuProfile::from_facts(facts, None);
        if listed.len() < MAX_GPUS && !listed.iter().any(|seen| same_model(seen, &gpu)) {
            listed.push(gpu);
        }
    }
    listed
}

/// What the wire's distinctness rule compares: the model, not the backend or driver.
fn same_model(a: &GpuProfile, b: &GpuProfile) -> bool {
    (a.vendor, a.vendor_id, a.device_id, a.generation, a.tier)
        == (b.vendor, b.vendor_id, b.device_id, b.generation, b.tier)
}

/// A device profile, in profile values, built from probed facts in one step (a test's or a
/// one-shot caller's route; the host uses [`DeviceContext`]).
pub fn profile_from(
    system: &SystemFacts,
    gpus: &[GpuFacts],
    path: RendererPath,
) -> Result<DeviceProfile, GatherError> {
    PlatformProfile::from_facts(system)?.profile(gpus, path)
}

/// Whether [`DeviceContext::update`] changed the held profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Update {
    /// A different profile is held now. The caller closes the window recorded under the old one
    /// and tells the process sampler ([`super::sampler::Sampler::profile_changed`]); nothing
    /// observed before is relabelled.
    Changed,
    /// The same profile as before.
    Unchanged,
}

#[derive(Debug)]
struct Held {
    generation: Generation,
    platform: PlatformProfile,
    profile: DeviceProfile,
}

/// The device profile of the current consent generation. Owned by the statistics worker.
#[derive(Debug, Default)]
pub struct DeviceContext {
    held: Option<Held>,
    revision: u64,
}

impl DeviceContext {
    pub const fn new() -> Self {
        Self {
            held: None,
            revision: 0,
        }
    }

    /// Gather, or recombine, the profile for the adapters and path reported now, probing the
    /// platform on the first call of a consent generation. Blocking (the probe): call it on a
    /// worker.
    pub fn update(
        &mut self,
        gate: &Gate,
        gpus: &[GpuFacts],
        path: RendererPath,
    ) -> Result<Update, GatherError> {
        self.update_with(gate, gpus, path, sys::probe)
    }

    /// [`Self::update`] with the platform probe supplied.
    pub fn update_with(
        &mut self,
        gate: &Gate,
        gpus: &[GpuFacts],
        path: RendererPath,
        probe: impl FnOnce() -> SystemFacts,
    ) -> Result<Update, GatherError> {
        let Some(generation) = gate.current() else {
            self.clear();
            return Err(GatherError::NotConsented);
        };
        if self
            .held
            .as_ref()
            .is_some_and(|h| h.generation != generation)
        {
            // Consent was revoked and given again: nothing carries over.
            self.held = None;
        }
        let gathered = match &self.held {
            Some(held) => Ok(held.platform),
            None => PlatformProfile::from_facts(&probe()),
        }
        .and_then(|platform| Ok((platform, platform.profile(gpus, path)?)));
        let (platform, profile) = match gathered {
            Ok(gathered) => gathered,
            Err(error) => {
                // A profile for the previous adapter must not stand for the new one.
                self.clear();
                return Err(error);
            }
        };
        if !gate.is_current(generation) {
            // Revoked while gathering: the result belongs to a consent that has ended.
            self.clear();
            return Err(GatherError::NotConsented);
        }
        let changed = self.held.as_ref().map(|held| &held.profile) != Some(&profile);
        self.held = Some(Held {
            generation,
            platform,
            profile,
        });
        if changed {
            self.revision += 1;
            Ok(Update::Changed)
        } else {
            Ok(Update::Unchanged)
        }
    }

    /// The held profile, only while the gate is still in the generation it was gathered in.
    pub fn profile(&self, gate: &Gate) -> Option<&DeviceProfile> {
        let held = self.held.as_ref()?;
        gate.is_current(held.generation).then_some(&held.profile)
    }

    /// How many different profiles this context has held; moves on every [`Update::Changed`].
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Forget everything: revocation, or a session that ended.
    pub fn clear(&mut self) {
        self.held = None;
    }

    /// Whether anything is held (for tests and diagnostics).
    pub fn is_empty(&self) -> bool {
        self.held.is_none()
    }
}

#[cfg(test)]
#[path = "device_tests.rs"]
mod tests;
