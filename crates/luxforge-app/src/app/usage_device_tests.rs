//! The desktop's graphics facts as device-profile inputs: path mapping, adapter reuse, and that
//! nothing is looked up or opened when the held adapters already say it.
use super::usage_device::{Observation, Pending, Resolved};
use crate::adapters::{Adapter, LaunchRenderer, Refusal};
use luxforge_core::{
    Renderer, RendererReason,
    usage::{
        DeviceProfile, Gate, RendererFallback,
        device::{DeviceContext, RendererPath, Update},
    },
};
use std::cell::Cell;

fn adapter(name: &str, backend: &str, vendor: u32, device: u32, kind: &str) -> Adapter {
    Adapter {
        name: name.into(),
        vendor,
        device,
        device_type: kind.into(),
        backend: backend.into(),
        driver: "driver".into(),
        driver_info: "Mesa 24.2.3".into(),
    }
}

fn m4() -> Adapter {
    adapter("Apple M4 Pro", "Metal", 0x106B, 0, "IntegratedGpu")
}

fn rtx() -> Adapter {
    adapter(
        "NVIDIA GeForce RTX 4070",
        "Vulkan",
        0x10DE,
        0x2786,
        "DiscreteGpu",
    )
}

fn window_of(adapter: &Adapter) -> Option<(String, String)> {
    Some((adapter.backend.clone(), adapter.name.clone()))
}

const GPU: LaunchRenderer = LaunchRenderer::Gpu { software: false };

#[test]
fn usage_the_path_follows_the_launch_refusal_then_the_session_reason() {
    let path = |launch, renderer| Observation::new(launch, renderer, None, None).path();
    assert_eq!(path(GPU, Renderer::gpu()), Ok(RendererPath::Gpu));
    assert_eq!(
        path(
            LaunchRenderer::Gpu { software: true },
            Renderer::gpu_software()
        ),
        Ok(RendererPath::Gpu),
        "a software adapter drawing through the GPU stage is still the GPU path"
    );
    for (refusal, fallback) in [
        (Refusal::Requested, RendererFallback::Disabled),
        (Refusal::SoftwareNotAdopted, RendererFallback::Unsupported),
        (Refusal::NoAdapter, RendererFallback::NoAdapter),
        (Refusal::GlBackend, RendererFallback::Unsupported),
    ] {
        assert_eq!(
            path(
                LaunchRenderer::Reference(refusal),
                Renderer::reference(RendererReason::NoAdapter)
            ),
            Ok(RendererPath::Reference(Some(fallback)))
        );
    }
    assert_eq!(
        path(GPU, Renderer::reference(RendererReason::NoAdapter)),
        Ok(RendererPath::Reference(Some(RendererFallback::NoAdapter)))
    );
    assert_eq!(
        path(GPU, Renderer::reference(RendererReason::DeviceLost)),
        Ok(RendererPath::Reference(Some(RendererFallback::AdapterLost)))
    );
    assert_eq!(
        path(GPU, Renderer::reference(RendererReason::Budget)),
        Ok(RendererPath::Reference(Some(RendererFallback::Other)))
    );
    assert_eq!(
        path(GPU, Renderer::reference(RendererReason::SurfacePending)),
        Err(Pending::SurfaceNotChecked),
        "the stage is not checked: no path is guessed"
    );
}

#[test]
fn usage_a_window_adapter_equal_to_the_tile_workers_is_not_looked_up() {
    let looked_up = Cell::new(0);
    let identify = |_: &str, _: &str| {
        looked_up.set(looked_up.get() + 1);
        None
    };
    let observation = Observation::new(GPU, Renderer::gpu(), Some(m4()), window_of(&m4()));
    let Resolved { gpus, path } = observation.resolve(identify).unwrap();
    assert_eq!(looked_up.get(), 0, "no enumeration for the same adapter");
    assert_eq!(gpus.len(), 1);
    assert_eq!(path, RendererPath::Gpu);
}

#[test]
fn usage_a_different_window_adapter_is_identified_once_and_listed_second() {
    let looked_up = Cell::new(0);
    let identify = |backend: &str, name: &str| {
        looked_up.set(looked_up.get() + 1);
        assert_eq!((backend, name), ("Metal", "Apple M4 Pro"));
        Some(m4())
    };
    let observation = Observation::new(GPU, Renderer::gpu(), Some(rtx()), window_of(&m4()));
    let resolved = observation.resolve(identify).unwrap();
    assert_eq!(looked_up.get(), 1);
    assert_eq!(resolved.gpus, [rtx().facts(), m4().facts()]);
}

#[test]
fn usage_without_a_tile_adapter_the_window_adapter_stands_and_an_unknown_one_is_omitted() {
    let only_window = Observation::new(GPU, Renderer::gpu(), None, window_of(&m4()));
    assert_eq!(
        only_window.resolve(|_, _| Some(m4())).unwrap().gpus,
        [m4().facts()]
    );
    // The lookup found nothing: the profile simply lists no GPU, with no invented one.
    assert!(only_window.resolve(|_, _| None).unwrap().gpus.is_empty());
    let nothing = Observation::new(
        LaunchRenderer::Reference(Refusal::Requested),
        Renderer::reference(RendererReason::Refused),
        None,
        None,
    );
    let resolved = nothing
        .resolve(|_, _| panic!("nothing to identify"))
        .unwrap();
    assert!(resolved.gpus.is_empty());
}

#[test]
fn usage_observations_are_equal_exactly_when_nothing_changed() {
    let a = Observation::new(GPU, Renderer::gpu(), Some(m4()), window_of(&m4()));
    assert_eq!(a, a.clone());
    let lost = Observation::new(
        GPU,
        Renderer::reference(RendererReason::DeviceLost),
        Some(m4()),
        window_of(&m4()),
    );
    assert_ne!(a, lost, "a path change is a change");
    let moved = Observation::new(GPU, Renderer::gpu(), Some(rtx()), window_of(&rtx()));
    assert_ne!(a, moved, "an adapter change is a change");
}

/// From held adapters to a valid profile and a profile change, through the core's context and
/// without opening anything.
#[test]
fn usage_an_adapter_change_gives_a_new_valid_profile() {
    let gate = Gate::new();
    gate.enable();
    let mut context = DeviceContext::new();
    let mut profile_of = |observation: &Observation| -> (Update, DeviceProfile) {
        let resolved = observation.resolve(|_, _| None).unwrap();
        let update = context
            .update(&gate, &resolved.gpus, resolved.path)
            .expect("this machine forms a profile");
        (update, *context.profile(&gate).unwrap())
    };
    let first = Observation::new(GPU, Renderer::gpu(), Some(m4()), window_of(&m4()));
    let (update, before) = profile_of(&first);
    assert_eq!(update, Update::Changed);
    assert_eq!(before.gpus().count(), 1);
    let (update, same) = profile_of(&first);
    assert_eq!(update, Update::Unchanged);
    assert_eq!(same, before);
    let lost = Observation::new(
        GPU,
        Renderer::reference(RendererReason::DeviceLost),
        Some(m4()),
        window_of(&m4()),
    );
    let (update, after) = profile_of(&lost);
    assert_eq!(update, Update::Changed);
    assert_eq!(
        after.renderer_fallback(),
        Some(RendererFallback::AdapterLost)
    );
    assert_ne!(after, before, "the new path is a new profile");
}

/// Facts carry numbers and enums only: nothing of an adapter's name or driver text survives into
/// the profile that is sent.
#[test]
fn usage_adapter_names_and_driver_text_do_not_reach_the_profile() {
    let secret = adapter(
        "Secret Prototype GPU",
        "Vulkan",
        0x1234,
        0x5678,
        "DiscreteGpu",
    );
    let observation = Observation::new(GPU, Renderer::gpu(), Some(secret), None);
    let resolved = observation.resolve(|_, _| None).unwrap();
    let gate = Gate::new();
    gate.enable();
    let mut context = DeviceContext::new();
    context
        .update(&gate, &resolved.gpus, resolved.path)
        .unwrap();
    let mut wire = Vec::new();
    context
        .profile(&gate)
        .unwrap()
        .write_json(&mut wire)
        .unwrap();
    let wire = String::from_utf8(wire).unwrap();
    for raw in ["Secret", "Prototype", "Mesa"] {
        assert!(!wire.contains(raw), "{raw} in {wire}");
    }
    assert!(wire.contains("\"vendor\":\"other\""));
}

/// This host's real adapter, as the launch enumerates it, through the observation to the profile:
/// within the 2 KiB cap, its vendor, backend and type as enums, an Apple silicon GPU's generation
/// and tier equal to the CPU brand's outside a virtual machine, and nothing of the adapter's name
/// or driver text on the wire. Enumeration creates a graphics instance and opens no device.
#[test]
#[luxforge_test_macros::slow]
fn usage_this_hosts_real_adapter_makes_a_profile_without_its_strings() {
    let offered = crate::adapters::enumerate(crate::adapters::renderer_backends());
    let Some(real) = offered
        .iter()
        .find(|adapter| adapter.device_type != "Cpu")
        .cloned()
    else {
        eprintln!("skipped: no GPU adapter on this host; not adapter evidence");
        return;
    };
    let observation = Observation::new(GPU, Renderer::gpu(), Some(real.clone()), window_of(&real));
    let resolved = observation
        .resolve(|_, _| panic!("the window's adapter is the tile worker's"))
        .unwrap();
    let gate = Gate::new();
    gate.enable();
    let mut context = DeviceContext::new();
    assert_eq!(
        context.update(&gate, &resolved.gpus, resolved.path),
        Ok(Update::Changed)
    );
    let profile = *context.profile(&gate).unwrap();
    assert!(profile.encoded_len() <= 2048, "{}", profile.encoded_len());
    let mut wire = Vec::new();
    profile.write_json(&mut wire).unwrap();
    let wire = String::from_utf8(wire).unwrap();
    let gpus: Vec<_> = profile.gpus().collect();
    assert_eq!(gpus.len(), 1, "{wire}");
    for raw in [&real.name, &real.driver, &real.driver_info] {
        for word in raw.split_whitespace().filter(|word| word.len() >= 3) {
            // The approved enum spellings, such as `apple` and `metal`, are lowercase.
            assert!(!wire.contains(word), "{word:?} of {raw:?} in {wire}");
        }
    }
    #[cfg(target_os = "macos")]
    {
        use luxforge_core::usage::{GpuBackend, GpuType, GpuVendor};
        let brand = std::process::Command::new("sysctl")
            .args(["-n", "machdep.cpu.brand_string"])
            .output()
            .unwrap();
        let brand = String::from_utf8(brand.stdout).unwrap();
        // A virtual Mac, such as a hosted CI runner, has an Apple CPU brand but exposes a
        // paravirtual Metal device, not the Apple silicon GPU.
        let hypervisor = std::process::Command::new("sysctl")
            .args(["-n", "kern.hv_vmm_present"])
            .output()
            .unwrap();
        let virtual_machine = String::from_utf8(hypervisor.stdout).unwrap().trim() == "1";
        if brand.starts_with("Apple M") && !virtual_machine {
            let gpu = gpus[0];
            assert_eq!(
                (gpu.vendor, gpu.backend, gpu.gpu_type, gpu.unified_memory),
                (
                    GpuVendor::Apple,
                    GpuBackend::Metal,
                    GpuType::Integrated,
                    true
                ),
                "{wire}"
            );
            assert_eq!(
                (gpu.generation, gpu.tier),
                (profile.cpu().generation, profile.cpu().tier),
                "{brand}: {wire}"
            );
        }
    }
    eprintln!("this host's profile ({} bytes): {wire}", wire.len());
}
