//! Device gathering: known and unknown platform and adapter fixtures, the GPU rules, consent and
//! revocation of the held profile, and this machine's real probe.
use super::*;
use crate::usage::{
    Collector, DeviceProfile, Event, EventKind, SessionClock, SessionStart, Trigger,
    encode::{BodyBuffer, Envelope, Release, ReportId, decode_batch, encode_batch},
};
use luxforge_gpu_types::adapter_facts::{AdapterDescription, normalize};
use luxforge_process::system_info::{
    Arch as SysArch, CpuFacts, CpuVendor as SysCpuVendor, LinuxDistro as SysDistro, MacOsBuild,
    MemoryBand as SysMemory, OsFacts, OsFamily as SysOs, Version as SysVersion, X86Signature,
};

fn version(parts: &[u32]) -> Option<SysVersion> {
    SysVersion::new(parts)
}

fn mac() -> SystemFacts {
    SystemFacts {
        os: OsFacts {
            family: SysOs::Macos,
            version: version(&[15, 5]),
            build: MacOsBuild::parse("24F74"),
            distro: None,
            distro_version: None,
            kernel: None,
        },
        arch: SysArch::Arm64,
        cpu: CpuFacts {
            vendor: SysCpuVendor::Apple,
            physical_cores: Some(12),
            logical_cores: Some(14),
            apple_generation: Some(4),
            apple_tier: Some(luxforge_process::system_info::AppleTier::Pro),
            x86: None,
        },
        memory: Some(SysMemory::Gib32),
    }
}

fn linux() -> SystemFacts {
    SystemFacts {
        os: OsFacts {
            family: SysOs::Linux,
            version: version(&[6, 8, 0]),
            build: None,
            distro: Some(SysDistro::Ubuntu),
            distro_version: version(&[24, 4]),
            kernel: version(&[6, 8, 0]),
        },
        arch: SysArch::X86_64,
        cpu: CpuFacts {
            vendor: SysCpuVendor::Intel,
            physical_cores: Some(8),
            logical_cores: Some(16),
            apple_generation: None,
            apple_tier: None,
            x86: Some(X86Signature {
                family: 6,
                model: 183,
                stepping: 1,
            }),
        },
        memory: Some(SysMemory::Gib64),
    }
}

/// A platform the probes recognised almost nothing of: only what the wire requires.
fn unknown() -> SystemFacts {
    SystemFacts {
        os: OsFacts {
            family: SysOs::Other,
            version: version(&[1]),
            build: None,
            distro: None,
            distro_version: None,
            kernel: None,
        },
        arch: SysArch::Other,
        cpu: CpuFacts {
            vendor: SysCpuVendor::Other,
            physical_cores: Some(1),
            logical_cores: Some(1),
            apple_generation: None,
            apple_tier: None,
            x86: None,
        },
        memory: Some(SysMemory::Over1024),
    }
}

fn adapter(
    name: &str,
    vendor: u32,
    device: u32,
    kind: &str,
    backend: &str,
    info: &str,
) -> GpuFacts {
    normalize(&AdapterDescription {
        name,
        vendor,
        device,
        device_type: kind,
        backend,
        driver: "",
        driver_info: info,
    })
}

fn m4_pro() -> GpuFacts {
    adapter("Apple M4 Pro", 0x106B, 0, "IntegratedGpu", "Metal", "")
}

fn rtx() -> GpuFacts {
    adapter(
        "NVIDIA GeForce RTX 4070",
        0x10DE,
        0x2786,
        "DiscreteGpu",
        "Vulkan",
        "560.35.03",
    )
}

fn intel_igpu() -> GpuFacts {
    adapter(
        "Intel(R) UHD Graphics",
        0x8086,
        0x4680,
        "IntegratedGpu",
        "Vulkan",
        "Mesa 24.2.3",
    )
}

fn json(profile: &DeviceProfile) -> String {
    let mut out = Vec::new();
    profile.write_json(&mut out).unwrap();
    String::from_utf8(out).unwrap()
}

/// Encode the profile in a batch and decode it with the strict validator.
fn round_trip(profile: &DeviceProfile) -> DeviceProfile {
    let release = Release::parse("0.0.0+0123456789ab").unwrap();
    let envelope = Envelope {
        release: &release,
        installation_id: ReportId::random(),
        session_id: ReportId::random(),
        session_start: SessionStart::parse("2026-10-10T10:00:00Z").unwrap(),
        batch_seq: 0,
        device: profile,
    };
    let mut body = BodyBuffer::new();
    let events = [Event {
        seq: 0,
        t: 0,
        kind: EventKind::SessionStart {
            trigger: Trigger::Launch,
        },
    }];
    encode_batch(&envelope, &events, 0, &mut body).expect("encodes");
    decode_batch(body.as_bytes()).expect("decodes").device
}

#[test]
fn usage_known_platforms_make_valid_profiles_that_round_trip() {
    let cases: [(SystemFacts, Vec<GpuFacts>, RendererPath); 4] = [
        (mac(), vec![m4_pro()], RendererPath::Gpu),
        (linux(), vec![rtx(), intel_igpu()], RendererPath::Gpu),
        (
            linux(),
            vec![],
            RendererPath::Reference(Some(RendererFallback::NoAdapter)),
        ),
        (
            unknown(),
            vec![adapter(
                "mystery",
                0,
                0,
                "Other",
                "Noop",
                "weird driver 1.x",
            )],
            RendererPath::Reference(None),
        ),
    ];
    for (system, gpus, path) in cases {
        let profile = profile_from(&system, &gpus, path).expect("a valid profile");
        assert!(profile.encoded_len() <= 2048);
        assert_eq!(round_trip(&profile), profile, "{}", json(&profile));
    }
}

#[test]
fn usage_known_values_reach_the_wire_as_numbers_and_enums() {
    let profile = profile_from(&mac(), &[m4_pro()], RendererPath::Gpu).unwrap();
    let wire: serde_json::Value = serde_json::from_str(&json(&profile)).unwrap();
    assert_eq!(wire["os"]["family"], "macos");
    assert_eq!(wire["os"]["version"], serde_json::json!([15, 5]));
    assert_eq!(wire["os"]["build"], "24F74");
    assert_eq!(wire["arch"], "arm64");
    assert_eq!(wire["cpu"]["vendor"], "apple");
    assert_eq!(wire["cpu"]["generation"], 4);
    assert_eq!(wire["cpu"]["tier"], "pro");
    assert_eq!(wire["cpu"]["physical_cores"], 12);
    assert_eq!(wire["cpu"]["logical_cores"], 14);
    assert_eq!(wire["memory_gib_band"], 32);
    assert_eq!(wire["gpus"][0]["vendor"], "apple");
    assert_eq!(wire["gpus"][0]["backend"], "metal");
    assert_eq!(wire["gpus"][0]["generation"], 4);
    assert_eq!(wire["gpus"][0]["unified_memory"], true);
    assert_eq!(wire["renderer"], "gpu");
    assert!(wire.get("renderer_fallback").is_none());

    let profile = profile_from(&linux(), &[rtx()], RendererPath::Gpu).unwrap();
    let wire: serde_json::Value = serde_json::from_str(&json(&profile)).unwrap();
    assert_eq!(wire["os"]["distro"], "ubuntu");
    assert_eq!(wire["os"]["kernel"], serde_json::json!([6, 8, 0]));
    assert_eq!(wire["cpu"]["family"], 6);
    assert_eq!(wire["cpu"]["model"], 183);
    assert_eq!(wire["gpus"][0]["vendor_id"], 0x10DE);
    assert_eq!(wire["gpus"][0]["device_id"], 0x2786);
    assert_eq!(
        wire["gpus"][0]["driver_version"],
        serde_json::json!([560, 35, 3])
    );
    assert_eq!(wire["gpus"][0]["unified_memory"], false);
}

#[test]
fn usage_an_unrecognised_adapter_is_other_and_leaves_no_raw_string() {
    let strange = adapter(
        "Totally Secret GPU Name 9000",
        0,
        0,
        "Other",
        "Noop",
        "driver build zz-private",
    );
    let profile = profile_from(&unknown(), &[strange], RendererPath::Reference(None)).unwrap();
    let text = json(&profile);
    for raw in ["Secret", "private", "driver", "Noop", "9000"] {
        assert!(!text.contains(raw), "{raw} in {text}");
    }
    let wire: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(wire["gpus"][0]["vendor"], "other");
    assert_eq!(wire["gpus"][0]["backend"], "other");
    assert_eq!(wire["gpus"][0]["type"], "other");
    for absent in [
        "vendor_id",
        "device_id",
        "driver_version",
        "generation",
        "tier",
    ] {
        assert!(wire["gpus"][0].get(absent).is_none(), "{absent}");
    }
    // Only keys the wire allows, at any depth of the device object.
    fn only_known(value: &serde_json::Value, allowed: &[&str]) {
        if let Some(object) = value.as_object() {
            for (key, inner) in object {
                assert!(allowed.contains(&key.as_str()), "{key}");
                only_known(inner, allowed);
            }
        }
    }
    only_known(
        &wire,
        &[
            "os",
            "family",
            "version",
            "arch",
            "cpu",
            "vendor",
            "physical_cores",
            "logical_cores",
            "memory_gib_band",
            "gpus",
            "backend",
            "type",
            "unified_memory",
            "renderer",
            "renderer_fallback",
        ],
    );
}

#[test]
fn usage_the_same_gpu_model_is_listed_once_and_at_most_two_are() {
    // The tile worker's adapter and the window's are often the same model.
    let listed = gpu_profiles(&[m4_pro(), m4_pro()]);
    assert_eq!(listed.len(), 1);
    // The same card seen through another backend is the same model.
    let gl_copy = adapter(
        "NVIDIA GeForce RTX 4070",
        0x10DE,
        0x2786,
        "DiscreteGpu",
        "Gl",
        "",
    );
    assert_eq!(gpu_profiles(&[rtx(), gl_copy]).len(), 1);

    let listed = gpu_profiles(&[rtx(), intel_igpu(), m4_pro()]);
    assert_eq!(listed.len(), 2, "never more than two");
    assert_eq!(listed[0].vendor_id, Some(0x10DE), "in the order given");
    assert_eq!(listed[1].vendor_id, Some(0x8086));

    // A duplicate does not take the second place from a different model.
    let listed = gpu_profiles(&[rtx(), rtx(), intel_igpu()]);
    assert_eq!(listed.len(), 2);
    assert_ne!(listed[0].vendor_id, listed[1].vendor_id);

    // The profile accepts the deduplicated list, which `DeviceProfile::new` would refuse raw.
    let profile = profile_from(&linux(), &[m4_pro(), m4_pro()], RendererPath::Gpu).unwrap();
    assert_eq!(profile.gpus().count(), 1);
    assert_eq!(gpu_profiles(&[]).len(), 0);
}

#[test]
fn usage_the_renderer_path_maps_to_the_wire_renderer_and_fallback() {
    for fallback in [
        RendererFallback::Disabled,
        RendererFallback::Unsupported,
        RendererFallback::NoAdapter,
        RendererFallback::AdapterLost,
        RendererFallback::Other,
    ] {
        let profile =
            profile_from(&mac(), &[m4_pro()], RendererPath::Reference(Some(fallback))).unwrap();
        assert_eq!(profile.renderer(), Renderer::Reference);
        assert_eq!(profile.renderer_fallback(), Some(fallback));
    }
    let gpu = profile_from(&mac(), &[m4_pro()], RendererPath::Gpu).unwrap();
    assert_eq!(gpu.renderer(), Renderer::Gpu);
    assert_eq!(gpu.renderer_fallback(), None);
    let bare = profile_from(&mac(), &[], RendererPath::Reference(None)).unwrap();
    assert_eq!(bare.renderer_fallback(), None);
}

#[test]
fn usage_a_missing_required_fact_is_named_not_invented() {
    let mut no_version = mac();
    no_version.os.version = None;
    let mut no_cores = mac();
    no_cores.cpu.logical_cores = None;
    let mut no_memory = mac();
    no_memory.memory = None;
    for (facts, what) in [
        (no_version, "the OS version"),
        (no_cores, "the CPU core counts"),
        (no_memory, "the installed memory"),
    ] {
        assert_eq!(
            profile_from(&facts, &[], RendererPath::Gpu),
            Err(GatherError::Missing(what))
        );
    }
    // A combination the wire refuses is an error, not a silently edited profile.
    let mut build_off_mac = linux();
    build_off_mac.os.build = MacOsBuild::parse("24F74");
    assert!(matches!(
        profile_from(&build_off_mac, &[], RendererPath::Gpu),
        Err(GatherError::Invalid(_))
    ));
}

#[test]
fn usage_the_widest_profile_still_fits_two_kib() {
    let mut system = linux();
    system.os.version = version(&[u32::MAX; 4]);
    system.os.distro_version = version(&[u32::MAX; 4]);
    system.os.kernel = version(&[u32::MAX; 4]);
    let mut first = rtx();
    first.driver_version = luxforge_gpu_types::adapter_facts::parse_driver_version(
        "4294967295.4294967295.4294967295.4294967295",
    );
    let mut second = intel_igpu();
    second.driver_version = first.driver_version;
    let profile = profile_from(
        &system,
        &[first, second],
        RendererPath::Reference(Some(RendererFallback::AdapterLost)),
    )
    .unwrap();
    assert!(profile.encoded_len() <= 2048, "{}", profile.encoded_len());
    assert_eq!(round_trip(&profile), profile);
}

fn panics() -> SystemFacts {
    panic!("nothing is probed without consent");
}

#[test]
fn usage_nothing_is_probed_without_consent() {
    let gate = Gate::new();
    let mut context = DeviceContext::new();
    assert_eq!(
        context.update_with(&gate, &[], RendererPath::Gpu, panics),
        Err(GatherError::NotConsented)
    );
    assert!(context.is_empty());
    assert_eq!(context.profile(&gate), None);
}

#[test]
fn usage_a_profile_is_gathered_once_and_changes_only_with_the_adapter() {
    let gate = Gate::new();
    gate.enable();
    let mut context = DeviceContext::new();
    let probes = std::cell::Cell::new(0);
    let probe = || {
        probes.set(probes.get() + 1);
        mac()
    };
    assert_eq!(
        context.update_with(&gate, &[m4_pro()], RendererPath::Gpu, probe),
        Ok(Update::Changed)
    );
    let first = *context.profile(&gate).expect("held");
    assert_eq!(context.revision(), 1);
    assert_eq!(
        context.update_with(&gate, &[m4_pro()], RendererPath::Gpu, probe),
        Ok(Update::Unchanged)
    );
    assert_eq!(context.revision(), 1);
    // The path moves to the reference after the device is lost: a new profile, no new probe.
    assert_eq!(
        context.update_with(
            &gate,
            &[m4_pro()],
            RendererPath::Reference(Some(RendererFallback::AdapterLost)),
            probe
        ),
        Ok(Update::Changed)
    );
    assert_eq!(probes.get(), 1, "the platform part is gathered once");
    assert_eq!(context.revision(), 2);
    assert_ne!(*context.profile(&gate).unwrap(), first);
    // The earlier value is untouched: observations made under it keep their own label.
    assert_eq!(first.renderer(), Renderer::Gpu);
}

#[test]
fn usage_revocation_clears_the_held_profile_at_once_and_a_new_opt_in_starts_over() {
    let gate = Gate::new();
    let first = gate.enable();
    let mut context = DeviceContext::new();
    let probes = std::cell::Cell::new(0);
    let probe = || {
        probes.set(probes.get() + 1);
        mac()
    };
    context
        .update_with(&gate, &[m4_pro()], RendererPath::Gpu, probe)
        .unwrap();
    assert!(context.profile(&gate).is_some());

    gate.disable();
    assert_eq!(
        context.profile(&gate),
        None,
        "invisible before anyone clears it"
    );
    assert_eq!(
        context.update_with(&gate, &[m4_pro()], RendererPath::Gpu, probe),
        Err(GatherError::NotConsented)
    );
    assert!(context.is_empty(), "the update drops it");

    let second = gate.enable();
    assert_ne!(first, second);
    context
        .update_with(&gate, &[m4_pro()], RendererPath::Gpu, probe)
        .unwrap();
    assert_eq!(probes.get(), 2, "the new consent period gathers again");

    // Off and on again without the worker noticing in between: the old profile is not carried
    // into the new generation.
    gate.disable();
    gate.enable();
    assert_eq!(context.profile(&gate), None);
    context
        .update_with(&gate, &[m4_pro()], RendererPath::Gpu, probe)
        .unwrap();
    assert_eq!(probes.get(), 3);
}

#[test]
fn usage_a_probe_that_overlaps_revocation_is_discarded() {
    let gate = Gate::new();
    gate.enable();
    let mut context = DeviceContext::new();
    let result = context.update_with(&gate, &[], RendererPath::Gpu, || {
        gate.disable();
        mac()
    });
    assert_eq!(result, Err(GatherError::NotConsented));
    assert!(context.is_empty());
    assert_eq!(context.profile(&gate), None);
}

#[test]
fn usage_a_failed_update_leaves_no_stale_profile() {
    let gate = Gate::new();
    gate.enable();
    let mut context = DeviceContext::new();
    let mut incomplete = mac();
    incomplete.memory = None;
    assert_eq!(
        context.update_with(&gate, &[], RendererPath::Gpu, || incomplete),
        Err(GatherError::Missing("the installed memory"))
    );
    assert_eq!(context.profile(&gate), None);
    // The platform is probed again on the next update, not remembered as failed or half-built.
    assert_eq!(
        context.update_with(&gate, &[], RendererPath::Gpu, mac),
        Ok(Update::Changed)
    );
}

/// This machine's real probe, through the real entry point, forms a valid profile.
#[test]
fn usage_this_machines_real_probe_makes_a_valid_profile() {
    let gate = Gate::new();
    gate.enable();
    let mut context = DeviceContext::new();
    let result = context.update(
        &gate,
        &[],
        RendererPath::Reference(Some(RendererFallback::Disabled)),
    );
    if cfg!(any(target_os = "macos", target_os = "linux", windows)) {
        assert_eq!(result, Ok(Update::Changed), "{result:?}");
        let profile = context.profile(&gate).unwrap();
        assert!(profile.encoded_len() <= 2048);
        assert_eq!(round_trip(profile), *profile);
        assert!(profile.cpu().logical_cores >= 1);
    }
}

/// Collection and a held profile are separate: stopping the collector does not need the context,
/// and a profile cannot outlive the generation it was gathered in.
#[test]
fn usage_the_collector_gate_governs_the_held_profile() {
    let collector = Collector::new();
    let started = SessionStart::parse("2026-10-10T10:00:00Z").unwrap();
    collector.start(
        Trigger::OptIn,
        started,
        SessionClock::manual().0,
        crate::usage::ImageKey::from_bytes([0; 32]),
    );
    let mut context = DeviceContext::new();
    context
        .update_with(collector.gate(), &[], RendererPath::Gpu, mac)
        .unwrap();
    assert!(context.profile(collector.gate()).is_some());
    collector.stop();
    assert!(context.profile(collector.gate()).is_none());
}

/// This Mac's real probe against what the system's own tools report, read here by the test (the
/// probe itself runs no command): the OS version and build, the architecture, the core counts,
/// the Apple silicon generation and tier from the CPU brand, and the memory band holding the
/// installed memory. The profile carries those values only as numbers and enums: no brand string,
/// product name, host name or user name appears in what it would send.
#[cfg(target_os = "macos")]
#[test]
fn usage_this_macs_profile_matches_what_the_system_reports_without_its_strings() {
    use crate::usage::{AppleTier, Arch, CpuVendor, MemoryBand, OsFamily};
    fn output(program: &str, args: &[&str]) -> String {
        let out = std::process::Command::new(program)
            .args(args)
            .output()
            .unwrap_or_else(|error| panic!("{program}: {error}"));
        assert!(out.status.success(), "{program} {args:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }
    let sysctl = |name: &str| output("sysctl", &["-n", name]);
    let gate = Gate::new();
    gate.enable();
    let mut context = DeviceContext::new();
    assert_eq!(
        context.update(&gate, &[], RendererPath::Gpu),
        Ok(Update::Changed)
    );
    let profile = *context.profile(&gate).unwrap();

    let os = profile.os();
    assert_eq!(os.family, OsFamily::Macos);
    let version: Vec<u32> = output("sw_vers", &["-productVersion"])
        .split('.')
        .map(|part| part.parse().unwrap())
        .collect();
    assert_eq!(os.version.parts(), version.as_slice());
    let build = output("sw_vers", &["-buildVersion"]);
    assert_eq!(os.build.map(|build| build.as_str().to_owned()), Some(build));

    let expected_arch = match std::env::consts::ARCH {
        "aarch64" => Arch::Arm64,
        "x86_64" => Arch::X86_64,
        _ => Arch::Other,
    };
    assert_eq!(profile.arch(), expected_arch);

    let cpu = profile.cpu();
    assert_eq!(
        u32::from(cpu.physical_cores),
        sysctl("hw.physicalcpu").parse::<u32>().unwrap()
    );
    assert_eq!(
        u32::from(cpu.logical_cores),
        sysctl("hw.logicalcpu").parse::<u32>().unwrap()
    );
    let brand = sysctl("machdep.cpu.brand_string");
    let words: Vec<&str> = brand.split_whitespace().collect();
    if let ["Apple", chip, rest @ ..] = words.as_slice()
        && let Some(generation) = chip.strip_prefix('M').and_then(|n| n.parse::<u8>().ok())
    {
        assert_eq!(cpu.vendor, CpuVendor::Apple, "{brand}");
        assert_eq!(cpu.generation, Some(generation), "{brand}");
        let tier = match rest {
            [] => Some(AppleTier::Base),
            ["Pro"] => Some(AppleTier::Pro),
            ["Max"] => Some(AppleTier::Max),
            ["Ultra"] => Some(AppleTier::Ultra),
            _ => None,
        };
        assert_eq!(cpu.tier, tier, "{brand}");
    }

    let gib = sysctl("hw.memsize")
        .parse::<u64>()
        .unwrap()
        .div_ceil(1 << 30);
    match profile.memory() {
        MemoryBand::Over1024 => assert!(gib > 1024),
        band => {
            let upper = u64::from(band.upper_gib().unwrap());
            assert!(
                gib <= upper && (upper == 8 || gib > upper / 2),
                "{gib} GiB in {band:?}"
            );
        }
    }

    // The values travel as numbers and enums only.
    let sent = json(&profile);
    let host = output("hostname", &[]);
    let user = std::env::var("USER").unwrap_or_default();
    for raw in [
        brand.as_str(),
        "macOS",
        "Mac OS",
        host.as_str(),
        user.as_str(),
    ] {
        if raw.len() >= 3 {
            assert!(!sent.contains(raw), "{raw:?} in {sent}");
        }
    }
    assert!(sent.len() <= 2048);
}
