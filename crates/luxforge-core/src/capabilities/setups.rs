//! Setups: the resources one tool needs, grouped into parts ([`SetupDescriptor`]). This file holds
//! what `module.status` reports of each ([`SetupRow`]), derived from the resource rows the status
//! already reads, the module's jobs and two facts of this host (its memory and the backends its
//! inference runtime offers), opening nothing; and what `module.setup.install` discloses when a
//! grant is missing ([`SetupDisclosure`]). The install itself is `host::setups`. See
//! `docs/design/module-capabilities.md#setups`.
use super::{
    consent::Disclosure,
    descriptor::{CapabilityKind, Introduction, SetupDescriptor, SetupPart},
    resources::{ResourceRow, ResourceState},
};
use crate::{
    JobId, JobStatus, ModuleDescriptor,
    jobs::{JobError, JobKind, JobRecord},
};
use serde::{Deserialize, Serialize};

/// One setup's state, as a tool's group shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SetupState {
    /// Every resource is installed.
    Installed,
    /// Something is not installed and nothing is installing it; a setup install would fetch it.
    Missing,
    /// An install of one of its resources is queued or running.
    Installing,
    /// The last install of a resource that is still missing failed; `error` is the first.
    Failed,
    /// A missing part cannot run on this host at all (`reason`), so no install is offered.
    Unavailable,
    /// This machine has less memory than the setup's minimum (`reason: memory`).
    CannotRun,
}

/// Why a setup is unavailable or cannot run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SetupReason {
    /// The host's inference runtime offers none of the backends a part's model runs on (or the
    /// host has no runtime), as on a host that compiles no generative runtime.
    NoRuntime,
    /// This machine has less memory than the setup's minimum.
    Memory,
}

/// One setup as `module.status` reports it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SetupRow {
    pub id: String,
    pub state: SetupState,
    /// The bytes of every resource that is not installed: what a setup install downloads, less
    /// what is staged.
    pub missing_bytes: u64,
    /// The bytes interrupted transfers of those resources have kept, which the install resumes.
    pub staged_bytes: u64,
    /// The largest of its parts' minimums: the least memory a machine needs to run the tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_memory: Option<u64>,
    /// Whether this machine meets the minimum; absent when there is none or the host cannot tell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fits: Option<bool>,
    pub parts: Vec<SetupPartRow>,
    /// The queued and running installs of its resources.
    #[serde(default)]
    pub jobs: Vec<SetupJob>,
    /// The first failure, in declared order, of a resource still missing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JobError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<SetupReason>,
}

/// One part of a setup as `module.status` reports it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SetupPartRow {
    pub name: String,
    pub state: SetupState,
    /// Every resource's bytes together, installed or not.
    pub bytes: u64,
    /// The bytes of its resources that are not installed.
    pub missing_bytes: u64,
    pub staged_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_memory: Option<u64>,
    pub resources: Vec<SetupResourceRow>,
}

/// One resource of a part: its state as `module.resource.list` reports it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SetupResourceRow {
    pub id: String,
    pub state: ResourceState,
    pub bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staged_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<JobId>,
}

/// A queued or running install of one of a setup's resources.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SetupJob {
    pub resource_id: String,
    pub job_id: JobId,
    pub status: JobStatus,
}

/// What a setup's status depends on beside the resource rows: this machine's memory and the
/// backends the host's inference runtime offers (`None` without a runtime).
#[derive(Clone, Debug, Default)]
pub(crate) struct HostFacts {
    pub(crate) machine_memory_bytes: Option<u64>,
    pub(crate) backends: Option<Vec<String>>,
}

impl HostFacts {
    /// Whether `resource`'s model can run on this host: a resource no `local-runtime` capability
    /// holds always can; one that is held, when the runtime offers a backend one of its holders
    /// names.
    pub(crate) fn runs(&self, module: &ModuleDescriptor, resource: &str) -> bool {
        let mut holders = module
            .capabilities
            .iter()
            .filter_map(|capability| match &capability.kind {
                CapabilityKind::LocalRuntime {
                    resource: held,
                    backends,
                    ..
                } if held == resource => Some(backends),
                _ => None,
            })
            .peekable();
        if holders.peek().is_none() {
            return true;
        }
        let offered = self.backends.as_deref().unwrap_or_default();
        holders
            .flatten()
            .any(|backend| offered.iter().any(|offered| offered == backend))
    }

    fn fits(&self, minimum: Option<u64>) -> Option<bool> {
        Some(self.machine_memory_bytes? >= minimum?)
    }
}

/// The least memory a part needs: the largest of its resources' minimums.
pub(crate) fn part_minimum(module: &ModuleDescriptor, part: &SetupPart) -> Option<u64> {
    part.resources
        .iter()
        .filter_map(|id| module.resource(id)?.min_memory_bytes)
        .max()
}

/// The least memory a setup needs: the largest of its parts' minimums.
pub(crate) fn setup_minimum(module: &ModuleDescriptor, setup: &SetupDescriptor) -> Option<u64> {
    setup
        .parts
        .iter()
        .filter_map(|part| part_minimum(module, part))
        .max()
}

/// The setup a resource's own download introduces itself by: the smallest setup that holds it
/// (the fewest resources, the first declared of equals), so a resource set up on its own reads the
/// words of the tool that needs least beside it.
pub(crate) fn introducing<'a>(
    module: &'a ModuleDescriptor,
    resource: &str,
) -> Option<&'a SetupDescriptor> {
    module
        .setups
        .iter()
        .filter(|setup| setup.resources().any(|held| held == resource))
        .min_by_key(|setup| setup.resources().count())
}

/// Every setup of `module` with its state, from `resources` (its resource rows, as
/// `module.status` reads them), `jobs` (the module's job records) and `host`.
///
/// A part is installed when all its resources are, installing while one is being installed,
/// unavailable when a missing resource's model cannot run on this host, cannot-run when this
/// machine is below its minimum, failed when a missing resource's last install failed, and
/// otherwise missing. The setup takes the same states over its parts in that order, except that
/// its memory check is over its own minimum, the largest of its parts'.
pub(crate) fn rows(
    module: &ModuleDescriptor,
    resources: &[ResourceRow],
    jobs: &[JobRecord],
    host: &HostFacts,
) -> Vec<SetupRow> {
    let row = |id: &str| resources.iter().find(|row| row.id == id);
    let status = |job_id: &JobId| {
        jobs.iter()
            .find(|job| &job.job_id == job_id && job.kind == JobKind::Install)
            .map(|job| job.status)
    };
    module
        .setups
        .iter()
        .map(|setup| {
            let mut first_error = None;
            let mut setup_jobs = Vec::new();
            let parts: Vec<SetupPartRow> = setup
                .parts
                .iter()
                .map(|part| {
                    let mut rows = Vec::with_capacity(part.resources.len());
                    let (mut bytes, mut missing, mut staged) = (0u64, 0u64, 0u64);
                    let (mut installing, mut failed, mut unavailable) = (false, false, false);
                    for id in &part.resources {
                        let Some(resource) = row(id) else { continue };
                        bytes = bytes.saturating_add(resource.bytes);
                        if resource.state != ResourceState::Installed {
                            missing = missing.saturating_add(resource.bytes);
                            staged = staged.saturating_add(resource.staged_bytes.unwrap_or(0));
                            unavailable |= !host.runs(module, id);
                        }
                        match resource.state {
                            ResourceState::Installing => {
                                installing = true;
                                if let Some(job_id) = &resource.job_id {
                                    setup_jobs.push(SetupJob {
                                        resource_id: id.clone(),
                                        job_id: job_id.clone(),
                                        status: status(job_id).unwrap_or(JobStatus::Queued),
                                    });
                                }
                            }
                            ResourceState::Failed => {
                                failed = true;
                                if first_error.is_none() {
                                    first_error = resource.error.clone();
                                }
                            }
                            ResourceState::Installed | ResourceState::NotInstalled => {}
                        }
                        rows.push(SetupResourceRow {
                            id: id.clone(),
                            state: resource.state,
                            bytes: resource.bytes,
                            staged_bytes: resource.staged_bytes,
                            job_id: resource.job_id.clone(),
                        });
                    }
                    let minimum = part_minimum(module, part);
                    let state = if missing == 0 {
                        SetupState::Installed
                    } else if installing {
                        SetupState::Installing
                    } else if unavailable {
                        SetupState::Unavailable
                    } else if host.fits(minimum) == Some(false) {
                        SetupState::CannotRun
                    } else if failed {
                        SetupState::Failed
                    } else {
                        SetupState::Missing
                    };
                    SetupPartRow {
                        name: part.name.clone(),
                        state,
                        bytes,
                        missing_bytes: missing,
                        staged_bytes: staged,
                        minimum_memory: minimum,
                        resources: rows,
                    }
                })
                .collect();
            let minimum = setup_minimum(module, setup);
            let fits = host.fits(minimum);
            let any = |state: SetupState| parts.iter().any(|part| part.state == state);
            let (state, reason) = if parts.iter().all(|part| part.state == SetupState::Installed) {
                (SetupState::Installed, None)
            } else if any(SetupState::Installing) {
                (SetupState::Installing, None)
            } else if any(SetupState::Unavailable) {
                (SetupState::Unavailable, Some(SetupReason::NoRuntime))
            } else if fits == Some(false) {
                (SetupState::CannotRun, Some(SetupReason::Memory))
            } else if any(SetupState::Failed) {
                (SetupState::Failed, None)
            } else {
                (SetupState::Missing, None)
            };
            SetupRow {
                id: setup.id.clone(),
                state,
                missing_bytes: parts.iter().map(|part| part.missing_bytes).sum(),
                staged_bytes: parts.iter().map(|part| part.staged_bytes).sum(),
                minimum_memory: minimum,
                fits,
                parts,
                jobs: setup_jobs,
                error: (state == SetupState::Failed)
                    .then_some(first_error)
                    .flatten(),
                reason,
            }
        })
        .collect()
}

/// What a person is told before setting up a tool: the setup's introduction and parts, and the
/// facts of every download it still needs summed over them (bytes, every origin, the licences, the
/// use restrictions and the minimum against this machine), with each missing download's own
/// disclosure beside them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupDisclosure {
    /// The module's title.
    pub module: String,
    /// The setup's identity and title.
    pub setup: String,
    pub title: String,
    pub introduction: Introduction,
    pub parts: Vec<PartDisclosure>,
    /// What the missing downloads fetch together.
    pub bytes: u64,
    /// Every origin they come from, in declared order.
    pub origins: Vec<String>,
    /// Their licences, each once.
    pub licenses: Vec<String>,
    /// Their use restrictions, each once.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub use_restrictions: Vec<String>,
    /// The setup's memory minimum, when a part declares one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_memory: Option<u64>,
    /// This machine's memory, beside a minimum, when the host can tell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_memory_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fits: Option<bool>,
    /// Each missing resource's download disclosure, in declared order.
    pub downloads: Vec<Disclosure>,
}

/// One part of a setup as its disclosure lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartDisclosure {
    pub name: String,
    pub purpose: String,
    /// Every resource's bytes together.
    pub bytes: u64,
    /// Every resource is installed already, so the part is not counted.
    pub installed: bool,
    pub resources: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::{
        descriptor::{CapabilityDescriptor, IntroductionPicture, ModelKind, ResourceDescriptor},
        testing::capability_descriptor,
    };

    const GIB: u64 = 1 << 30;

    /// A module of two resources, `finder` (no minimum) and `fill` (8 GiB, a model run on
    /// `metal`), and two setups: `object`, Finder then Fill, and `masks`, Finder alone.
    fn module() -> ModuleDescriptor {
        let mut module = capability_descriptor();
        let resource = |id: &str, bytes: u64, minimum: Option<u64>| ResourceDescriptor {
            id: id.into(),
            bytes,
            min_memory_bytes: minimum,
            ..module.resources[0].clone()
        };
        module.resources = vec![
            resource("finder", 100, None),
            resource("fill", 1000, Some(8 * GIB)),
        ];
        module.tasks.clear();
        module.controls.clear();
        module.capabilities = ["finder", "fill"]
            .map(|id| CapabilityDescriptor {
                id: format!("download-{id}"),
                kind: CapabilityKind::DownloadArtifact {
                    resource: id.into(),
                },
                purpose: "Install it.".into(),
            })
            .into();
        module.capabilities.push(CapabilityDescriptor {
            id: "run-fill".into(),
            kind: CapabilityKind::LocalRuntime {
                resource: "fill".into(),
                model: ModelKind::Generator,
                resident_bytes: GIB,
                backends: vec!["metal".into()],
                precision: crate::inference::Precision::Fp32,
                with: Vec::new(),
            },
            purpose: "Fill.".into(),
        });
        let introduction = Introduction {
            eyebrow: "Test".into(),
            title: "Point".into(),
            lead: "Point at it.".into(),
            hero: Some(IntroductionPicture::Hover),
            steps: Vec::new(),
            promise: "Stays here.".into(),
        };
        let part = |name: &str, resource: &str| SetupPart {
            name: name.into(),
            purpose: "Does it.".into(),
            resources: vec![resource.into()],
        };
        module.setups = vec![
            SetupDescriptor {
                id: "object".into(),
                title: "Object".into(),
                introduction: introduction.clone(),
                parts: vec![part("Finder", "finder"), part("Fill", "fill")],
            },
            SetupDescriptor {
                id: "masks".into(),
                title: "Masks".into(),
                introduction,
                parts: vec![part("Finder", "finder")],
            },
        ];
        module.validate().expect("the test module validates");
        module
    }

    fn resource_row(id: &str, bytes: u64, state: ResourceState) -> ResourceRow {
        ResourceRow {
            id: id.into(),
            title: id.into(),
            version: "1".into(),
            bytes,
            sha256: String::new(),
            license: "CC0-1.0".into(),
            provenance: String::new(),
            url: String::new(),
            files: Vec::new(),
            tradeoff: None,
            use_restriction: None,
            requirements: None,
            fits: None,
            state,
            staged_bytes: None,
            path: None,
            installed_ms: None,
            job_id: None,
            error: None,
        }
    }

    fn host(memory: u64, backends: &[&str]) -> HostFacts {
        HostFacts {
            machine_memory_bytes: Some(memory),
            backends: Some(backends.iter().map(|b| (*b).to_owned()).collect()),
        }
    }

    /// The `object` setup's row over these two resource rows.
    fn object(rows: &[ResourceRow], jobs: &[JobRecord], host: &HostFacts) -> SetupRow {
        let module = module();
        super::rows(&module, rows, jobs, host).remove(0)
    }

    #[test]
    fn a_setup_reports_each_state_from_its_resources_and_this_host() {
        use ResourceState::{Failed, Installed, Installing, NotInstalled};
        let big = host(16 * GIB, &["metal"]);
        let pair = |finder, fill| {
            [
                resource_row("finder", 100, finder),
                resource_row("fill", 1000, fill),
            ]
        };

        // Nothing installed: missing, every byte counted, staged bytes summed.
        let mut rows = pair(NotInstalled, NotInstalled);
        rows[1].staged_bytes = Some(400);
        let missing = object(&rows, &[], &big);
        assert_eq!(
            (missing.state, missing.missing_bytes, missing.staged_bytes),
            (SetupState::Missing, 1100, 400)
        );
        assert_eq!(
            (missing.minimum_memory, missing.fits),
            (Some(8 * GIB), Some(true))
        );
        assert_eq!(missing.parts[1].minimum_memory, Some(8 * GIB));

        // The finder installed: only the fill counts, and the finder's part is installed.
        let partly = object(&pair(Installed, NotInstalled), &[], &big);
        assert_eq!(
            (partly.state, partly.missing_bytes),
            (SetupState::Missing, 1000)
        );
        assert_eq!(partly.parts[0].state, SetupState::Installed);
        assert_eq!(partly.parts[0].bytes, 100);

        // Everything installed.
        let done = object(&pair(Installed, Installed), &[], &big);
        assert_eq!((done.state, done.missing_bytes), (SetupState::Installed, 0));

        // An install queued or running: installing, with its job.
        let job = JobId::new();
        let mut rows = pair(Installed, Installing);
        rows[1].job_id = Some(job.clone());
        let installing = object(&rows, &[], &big);
        assert_eq!(installing.state, SetupState::Installing);
        assert_eq!(installing.jobs.len(), 1);
        assert_eq!(
            (
                installing.jobs[0].resource_id.as_str(),
                &installing.jobs[0].job_id
            ),
            ("fill", &job)
        );

        // A failed install: failed with the first failure, in declared order.
        let mut rows = pair(Failed, Failed);
        rows[0].error = Some(JobError {
            code: "read-error".into(),
            message: "the finder's connection was reset".into(),
            data: None,
        });
        rows[1].error = Some(JobError {
            code: "validation".into(),
            message: "the fill did not match its pin".into(),
            data: None,
        });
        let failed = object(&rows, &[], &big);
        assert_eq!(failed.state, SetupState::Failed);
        assert_eq!(
            failed.error.as_ref().map(|error| error.code.as_str()),
            Some("read-error")
        );

        // No runtime for the fill's backend: unavailable, and the finder alone stays missing.
        let elsewhere = host(16 * GIB, &["cpu"]);
        let unavailable = object(&pair(NotInstalled, NotInstalled), &[], &elsewhere);
        assert_eq!(
            (unavailable.state, unavailable.reason),
            (SetupState::Unavailable, Some(SetupReason::NoRuntime))
        );
        assert_eq!(unavailable.parts[0].state, SetupState::Missing);
        let none = HostFacts {
            backends: None,
            ..elsewhere.clone()
        };
        assert_eq!(
            object(&pair(NotInstalled, NotInstalled), &[], &none).state,
            SetupState::Unavailable
        );
        let module = module();
        let masks = &super::rows(&module, &pair(NotInstalled, NotInstalled), &[], &none)[1];
        assert_eq!(masks.state, SetupState::Missing, "the finder runs anywhere");

        // A machine below the minimum: cannot-run, whatever has failed.
        let small = host(4 * GIB, &["metal"]);
        let cannot = object(&pair(NotInstalled, Failed), &[], &small);
        assert_eq!(
            (cannot.state, cannot.reason, cannot.fits),
            (
                SetupState::CannotRun,
                Some(SetupReason::Memory),
                Some(false)
            )
        );
        assert_eq!(cannot.error, None);
        let unknown = HostFacts {
            machine_memory_bytes: None,
            ..small
        };
        let unknown = object(&pair(NotInstalled, NotInstalled), &[], &unknown);
        assert_eq!(
            (unknown.state, unknown.fits),
            (SetupState::Missing, None),
            "a host that cannot tell its memory checks nothing"
        );
    }

    #[test]
    fn a_download_introduces_itself_by_its_smallest_setup() {
        let module = module();
        assert_eq!(
            introducing(&module, "finder").map(|s| s.id.as_str()),
            Some("masks")
        );
        assert_eq!(
            introducing(&module, "fill").map(|s| s.id.as_str()),
            Some("object")
        );
        assert!(introducing(&capability_descriptor(), "palette").is_none());
    }
}
