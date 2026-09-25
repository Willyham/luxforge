//! What the operating system accounts to this process, and the working-memory budgets of the
//! render context every evaluation the owner plans shares, as `resources.read` reports them.
//!
//! The counters belong to the process, not to a catalog owner, so there is one sampler per process;
//! the budgets belong to the render context the caller passes in. A read takes its mutex on the
//! owner thread and costs a handful of system calls plus, with this process's GPU clients cached, a
//! few IORegistry property reads: bookkeeping, not frame work.
use crate::RenderContext;
pub use lightwell_process::MemoryKind;
use lightwell_process::{Counters, Sampler, Unavailable};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    sync::{Mutex, MutexGuard, OnceLock, PoisonError},
    time::Instant,
};

struct Shared {
    sampler: Sampler,
    /// The origin of `monotonic_ns`: the first use of the counters in this process.
    epoch: Instant,
}

static SHARED: OnceLock<Mutex<Shared>> = OnceLock::new();

fn shared() -> MutexGuard<'static, Shared> {
    SHARED
        .get_or_init(|| {
            Mutex::new(Shared {
                sampler: Sampler::new(),
                epoch: Instant::now(),
            })
        })
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Declare that this process presents through the GPU, so GPU allocations are read from here on.
/// They come from the system default Metal device, and asking for that device creates one in a
/// process that has none; a process that presents already has it, since it is the device wgpu
/// uses. The desktop calls this once at startup; the headless owner never does, and reports
/// allocations unavailable with the reason. Idempotent.
pub fn declare_gpu_presenter() {
    shared().sampler.enable_gpu_allocations();
}

/// The counters a platform cannot give, by key, each with its reason.
pub type Reasons = BTreeMap<&'static str, &'static str>;

/// One `resources.read`. Every time and byte counter is cumulative or a level at the read, so a
/// rate comes from two reports: CPU percent of one core is
/// `100 × Δcpu.time_ns / Δmonotonic_ns`, and GPU percent is the same over `gpu.time_ns`. A counter
/// the platform cannot give is omitted and named, with its reason, in its object's `unavailable`,
/// which is itself omitted when nothing is missing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ResourceReport {
    /// Nanoseconds on a monotonic clock, taken immediately before the counters. Only a difference
    /// between two reports means anything.
    pub monotonic_ns: u64,
    pub cpu: CpuReport,
    pub memory: MemoryReport,
    pub gpu: GpuReport,
    pub budgets: BudgetsReport,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CpuReport {
    /// User plus system CPU time of every thread since the process started.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_ns: Option<u64>,
    /// The ceiling a CPU rate is read against: 14 logical CPUs can reach 1400% of one core.
    pub logical_cpus: u32,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub unavailable: Reasons,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MemoryReport {
    /// Which measure `bytes` is: the one the platform's own monitor shows.
    pub kind: MemoryKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peak_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resident_bytes: Option<u64>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub unavailable: Reasons,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GpuReport {
    /// GPU time spent on this process's work since it started. It never decreases.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_ns: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allocated_bytes: Option<u64>,
    /// Whether GPU allocations share system memory and so are part of `memory.bytes`; omitted
    /// when no device was read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unified_memory: Option<bool>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub unavailable: Reasons,
}

/// A render context's working-memory budgets: exact, and free to read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BudgetsReport {
    pub colour_scratch: BudgetReport,
    pub spatial: BudgetReport,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct BudgetReport {
    pub target_bytes: u64,
    pub in_use_bytes: u64,
    /// The high-water mark of `in_use_bytes` since the process started, which may exceed the
    /// target: a budget is a target, not a limit.
    pub peak_bytes: u64,
}

/// Read every counter now, and the budgets of `context`.
pub fn read(context: &RenderContext) -> ResourceReport {
    let (monotonic_ns, counters) = {
        let mut shared = shared();
        let since = shared.epoch.elapsed();
        (
            u64::try_from(since.as_nanos()).unwrap_or(u64::MAX),
            shared.sampler.read(),
        )
    };
    let scratch = context.scratch();
    let spatial = context.spatial();
    report(
        monotonic_ns,
        counters,
        BudgetsReport {
            colour_scratch: BudgetReport {
                target_bytes: scratch.target(),
                in_use_bytes: scratch.in_use(),
                peak_bytes: scratch.peak(),
            },
            spatial: BudgetReport {
                target_bytes: spatial.target(),
                in_use_bytes: spatial.in_use(),
                peak_bytes: spatial.peak(),
            },
        },
    )
}

/// Arrange one read of the counters as the report: a value in its field, or its key and reason in
/// `unavailable`, never both and never a zero standing in for "unknown".
fn report(monotonic_ns: u64, counters: Counters, budgets: BudgetsReport) -> ResourceReport {
    fn split(
        key: &'static str,
        value: Result<u64, Unavailable>,
        unavailable: &mut Reasons,
    ) -> Option<u64> {
        match value {
            Ok(value) => Some(value),
            Err(Unavailable(reason)) => {
                unavailable.insert(key, reason);
                None
            }
        }
    }
    let mut cpu = Reasons::new();
    let mut memory = Reasons::new();
    let mut gpu = Reasons::new();
    ResourceReport {
        monotonic_ns,
        cpu: CpuReport {
            time_ns: split("time_ns", counters.cpu_time_ns, &mut cpu),
            logical_cpus: counters.logical_cpus,
            unavailable: cpu,
        },
        memory: MemoryReport {
            kind: counters.memory.kind,
            bytes: split("bytes", counters.memory.bytes, &mut memory),
            peak_bytes: split("peak_bytes", counters.memory.peak_bytes, &mut memory),
            resident_bytes: split(
                "resident_bytes",
                counters.memory.resident_bytes,
                &mut memory,
            ),
            unavailable: memory,
        },
        gpu: GpuReport {
            time_ns: split("time_ns", counters.gpu.time_ns, &mut gpu),
            allocated_bytes: split("allocated_bytes", counters.gpu.allocated_bytes, &mut gpu),
            unified_memory: counters.gpu.unified_memory,
            unavailable: gpu,
        },
        budgets,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ApiRequest, OwnerHandle};
    use lightwell_process::{Gpu, Memory};
    use serde_json::{Value, json};

    fn keys(value: &Value) -> Vec<&str> {
        let mut keys: Vec<&str> = value
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        keys
    }

    /// Each counter is a number in its field or a reason under its key in `unavailable`, never
    /// both and never missing from both.
    fn accounted(object: &Value, counters: &[&str]) {
        for counter in counters {
            let value = object.get(*counter);
            let reason = object
                .get("unavailable")
                .and_then(|reasons| reasons.get(*counter));
            assert!(
                value.is_some_and(Value::is_u64)
                    != reason.is_some_and(|r| r.as_str().is_some_and(|r| !r.is_empty())),
                "{counter} in {object}"
            );
        }
        if let Some(reasons) = object.get("unavailable") {
            assert!(
                !reasons.as_object().expect("reasons by key").is_empty(),
                "an empty unavailable object is omitted"
            );
        }
    }

    /// A real read on this machine has exactly the documented shape.
    #[test]
    fn a_read_has_the_documented_keys() {
        let report = serde_json::to_value(read(&crate::RenderContext::new())).unwrap();
        assert_eq!(
            keys(&report),
            ["budgets", "cpu", "gpu", "memory", "monotonic_ns"]
        );
        assert!(report["monotonic_ns"].is_u64());
        assert!(
            report["cpu"]["logical_cpus"]
                .as_u64()
                .is_some_and(|cpus| cpus >= 1)
        );
        accounted(&report["cpu"], &["time_ns"]);
        accounted(
            &report["memory"],
            &["bytes", "peak_bytes", "resident_bytes"],
        );
        accounted(&report["gpu"], &["time_ns", "allocated_bytes"]);
        for (object, documented) in [
            ("cpu", &["time_ns", "logical_cpus", "unavailable"][..]),
            (
                "memory",
                &[
                    "kind",
                    "bytes",
                    "peak_bytes",
                    "resident_bytes",
                    "unavailable",
                ],
            ),
            (
                "gpu",
                &[
                    "time_ns",
                    "allocated_bytes",
                    "unified_memory",
                    "unavailable",
                ],
            ),
        ] {
            for key in keys(&report[object]) {
                assert!(
                    documented.contains(&key),
                    "{object}.{key} is not documented"
                );
            }
        }
        assert!(
            ["footprint", "resident", "private"]
                .contains(&report["memory"]["kind"].as_str().unwrap())
        );
        for budget in ["colour_scratch", "spatial"] {
            assert_eq!(
                keys(&report["budgets"][budget]),
                ["in_use_bytes", "peak_bytes", "target_bytes"],
                "{budget}"
            );
        }
        assert_eq!(
            report["budgets"]["colour_scratch"]["target_bytes"],
            json!(crate::RenderContext::new().scratch().target())
        );
        assert_eq!(
            report["budgets"]["spatial"]["target_bytes"],
            json!(crate::RenderContext::new().spatial().target())
        );
        if cfg!(any(target_os = "macos", target_os = "linux", windows)) {
            assert!(report["cpu"].get("unavailable").is_none(), "{report}");
            assert!(report["memory"].get("unavailable").is_none(), "{report}");
        }
        if cfg!(target_os = "macos") {
            assert_eq!(report["memory"]["kind"], "footprint");
            // Nothing in this test binary declares a GPU presenter, as nothing in the headless
            // owner does.
            assert_eq!(
                report["gpu"]["unavailable"]["allocated_bytes"],
                "no GPU presenter in this process"
            );
            assert!(report["gpu"].get("unified_memory").is_none());
        }
        let later = serde_json::to_value(read(&crate::RenderContext::new())).unwrap();
        assert!(later["monotonic_ns"].as_u64() > report["monotonic_ns"].as_u64());
    }

    /// Unavailable counters are omitted with their reasons, each in its own object, and an object
    /// with nothing missing has no `unavailable` at all.
    #[test]
    fn unavailable_counters_are_omitted_and_named_with_their_reasons() {
        let budget = BudgetReport {
            target_bytes: 64,
            in_use_bytes: 0,
            peak_bytes: 3,
        };
        let counters = Counters {
            cpu_time_ns: Ok(5_231_200_000),
            logical_cpus: 14,
            memory: Memory {
                kind: MemoryKind::Resident,
                bytes: Ok(980),
                peak_bytes: Err(Unavailable("no peak here")),
                resident_bytes: Ok(980),
            },
            gpu: Gpu {
                time_ns: Err(Unavailable("GPU time is not reported on Linux yet")),
                allocated_bytes: Err(Unavailable("GPU allocations are not reported on Linux yet")),
                unified_memory: None,
            },
        };
        let value = serde_json::to_value(report(
            81_234_567_000,
            counters,
            BudgetsReport {
                colour_scratch: budget,
                spatial: budget,
            },
        ))
        .unwrap();
        assert_eq!(
            value,
            json!({
                "monotonic_ns": 81_234_567_000_u64,
                "cpu": {"time_ns": 5_231_200_000_u64, "logical_cpus": 14},
                "memory": {
                    "kind": "resident", "bytes": 980, "resident_bytes": 980,
                    "unavailable": {"peak_bytes": "no peak here"}
                },
                "gpu": {"unavailable": {
                    "time_ns": "GPU time is not reported on Linux yet",
                    "allocated_bytes": "GPU allocations are not reported on Linux yet"
                }},
                "budgets": {
                    "colour_scratch": {"target_bytes": 64, "in_use_bytes": 0, "peak_bytes": 3},
                    "spatial": {"target_bytes": 64, "in_use_bytes": 0, "peak_bytes": 3}
                }
            })
        );
    }

    /// Through the owner, as any client calls it: the read answers, needs no asset, and leaves the
    /// event log untouched, so a monitor polling it never makes another client resynchronise.
    #[test]
    fn resources_read_through_the_owner_emits_no_event() {
        let catalog =
            std::env::temp_dir().join(format!("lightwell-resources-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&catalog);
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
        let client = owner.register();
        let call = |method: &str, params: Value| {
            owner
                .call(
                    client,
                    ApiRequest {
                        id: method.into(),
                        method: method.into(),
                        params,
                        token: None,
                    },
                )
                .expect("the owner answered")
        };
        for _ in 0..3 {
            let read = call("resources.read", json!({}));
            assert!(read.error.is_none(), "{:?}", read.error);
            assert_eq!(read.sequence, 0, "no mutation, so the sequence stays");
            accounted(&read.result.unwrap()["memory"], &["bytes"]);
        }
        let events = call("events.since", json!({"after": 0})).result.unwrap();
        assert_eq!(events["events"], json!([]));
        assert_eq!(events["current_sequence"], 0);
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
    }
}
