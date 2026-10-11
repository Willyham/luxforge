use super::*;

fn adapter<'a>(
    name: &'a str,
    vendor: u32,
    device: u32,
    device_type: &'a str,
    backend: &'a str,
    driver_info: &'a str,
) -> AdapterDescription<'a> {
    AdapterDescription {
        name,
        vendor,
        device,
        device_type,
        backend,
        driver: "",
        driver_info,
    }
}

fn version(facts: &GpuFacts) -> Option<Vec<u32>> {
    facts.driver_version.map(|v| v.parts().to_vec())
}

#[test]
fn apple_metal_is_unified_with_generation_and_tier() {
    let facts = normalize(&adapter(
        "Apple M4 Pro",
        0x106B,
        0,
        "IntegratedGpu",
        "Metal",
        "",
    ));
    assert_eq!(facts.vendor, GpuVendor::Apple);
    assert_eq!(facts.backend, GpuBackend::Metal);
    assert_eq!(facts.device_type, GpuType::Integrated);
    assert!(facts.unified_memory);
    assert_eq!(
        (facts.apple_generation, facts.apple_tier),
        (Some(4), Some(AppleTier::Pro))
    );
    assert_eq!((facts.vendor_id, facts.device_id), (Some(0x106B), None));
    assert_eq!(facts.driver_version, None);

    // The vendor id may be absent; the name still identifies it.
    let facts = normalize(&adapter("Apple M2", 0, 0, "IntegratedGpu", "Metal", ""));
    assert_eq!(facts.vendor, GpuVendor::Apple);
    assert_eq!(
        (facts.apple_generation, facts.apple_tier),
        (Some(2), Some(AppleTier::Base))
    );
    assert_eq!((facts.vendor_id, facts.device_id), (None, None));

    for (name, generation, tier) in [
        ("Apple M1", 1, AppleTier::Base),
        ("Apple M3 Max", 3, AppleTier::Max),
        ("Apple M4 Ultra", 4, AppleTier::Ultra),
    ] {
        let facts = normalize(&adapter(name, 0x106B, 0, "IntegratedGpu", "Metal", ""));
        assert_eq!(
            (facts.apple_generation, facts.apple_tier),
            (Some(generation), Some(tier))
        );
    }
    // An unknown Apple name is still an Apple adapter by vendor id, with no model.
    let facts = normalize(&adapter(
        "Apple Silicon Next",
        0x106B,
        0,
        "IntegratedGpu",
        "Metal",
        "",
    ));
    assert_eq!(facts.vendor, GpuVendor::Apple);
    assert_eq!((facts.apple_generation, facts.apple_tier), (None, None));
}

#[test]
fn nvidia_vulkan_keeps_pci_ids_and_a_numeric_driver() {
    let facts = normalize(&adapter(
        "NVIDIA GeForce RTX 4070",
        0x10DE,
        0x2786,
        "DiscreteGpu",
        "Vulkan",
        "560.35.03",
    ));
    assert_eq!(facts.vendor, GpuVendor::Nvidia);
    assert_eq!(facts.backend, GpuBackend::Vulkan);
    assert_eq!(facts.device_type, GpuType::Discrete);
    assert!(!facts.unified_memory);
    assert_eq!(
        (facts.vendor_id, facts.device_id),
        (Some(0x10DE), Some(0x2786))
    );
    assert_eq!(version(&facts), Some(vec![560, 35, 3]));
    assert_eq!((facts.apple_generation, facts.apple_tier), (None, None));
}

#[test]
fn amd_and_intel_adapters() {
    let amd = normalize(&adapter(
        "AMD Radeon RX 7800 XT (RADV NAVI32)",
        0x1002,
        0x747E,
        "DiscreteGpu",
        "Vulkan",
        "Mesa 24.2.3-1ubuntu1",
    ));
    assert_eq!(amd.vendor, GpuVendor::Amd);
    assert_eq!(version(&amd), Some(vec![24, 2, 3]));

    let radeon_igpu = normalize(&adapter(
        "AMD Radeon 780M",
        0x1002,
        0x15BF,
        "IntegratedGpu",
        "Dx12",
        "32.0.21013.1000",
    ));
    assert!(radeon_igpu.unified_memory);
    assert_eq!(radeon_igpu.backend, GpuBackend::Dx12);
    assert_eq!(version(&radeon_igpu), Some(vec![32, 0, 21013, 1000]));

    let intel = normalize(&adapter(
        "Intel(R) Iris(R) Xe Graphics",
        0x8086,
        0x9A49,
        "IntegratedGpu",
        "Vulkan",
        "Mesa 24.0.5 (LLVM 17.0.6)",
    ));
    assert_eq!(intel.vendor, GpuVendor::Intel);
    assert!(intel.unified_memory);
    assert_eq!(
        (intel.vendor_id, intel.device_id),
        (Some(0x8086), Some(0x9A49))
    );
    assert_eq!(version(&intel), Some(vec![24, 0, 5]));
}

#[test]
fn software_rasterizers_are_cpu_type_and_mesa_or_microsoft() {
    let llvmpipe = normalize(&adapter(
        "llvmpipe (LLVM 17.0.6, 256 bits)",
        0x10005,
        0,
        "Cpu",
        "Vulkan",
        "Mesa 24.0.5",
    ));
    assert_eq!(llvmpipe.vendor, GpuVendor::Mesa);
    assert_eq!(llvmpipe.device_type, GpuType::Cpu);
    assert!(llvmpipe.unified_memory);
    // 0x10005 is not a PCI id.
    assert_eq!((llvmpipe.vendor_id, llvmpipe.device_id), (None, None));
    assert_eq!(version(&llvmpipe), Some(vec![24, 0, 5]));

    // The same rasterizer through GL, with a different vendor id, is still Mesa's.
    let gl = normalize(&adapter(
        "llvmpipe (LLVM 15.0.7, 256 bits)",
        0,
        0,
        "Cpu",
        "Gl",
        "",
    ));
    assert_eq!((gl.vendor, gl.backend), (GpuVendor::Mesa, GpuBackend::Gl));

    let lavapipe = normalize(&adapter("lavapipe", 0x10005, 0, "Cpu", "Vulkan", ""));
    assert_eq!(lavapipe.vendor, GpuVendor::Mesa);

    let warp = normalize(&adapter(
        "Microsoft Basic Render Driver",
        0x1414,
        0x8C,
        "Cpu",
        "Dx12",
        "",
    ));
    assert_eq!(warp.vendor, GpuVendor::Microsoft);
    assert_eq!(warp.device_type, GpuType::Cpu);
    assert_eq!((warp.vendor_id, warp.device_id), (Some(0x1414), Some(0x8C)));
}

#[test]
fn other_vendors_and_unknowns_use_the_unknown_enums() {
    for (id, vendor) in [
        (0x5143, GpuVendor::Qualcomm),
        (0x13B5, GpuVendor::Arm),
        (0x1010, GpuVendor::Imagination),
        (0x1234, GpuVendor::Other),
        (0, GpuVendor::Other),
        (0x10001, GpuVendor::Other),
    ] {
        assert_eq!(
            normalize(&adapter("x", id, 1, "Other", "Vulkan", "")).vendor,
            vendor,
            "{id:#x}"
        );
    }
    let facts = normalize(&adapter(
        "Secret Name",
        0x1234,
        7,
        "Strange",
        "Webgpu",
        "internal build 9",
    ));
    assert_eq!(facts.backend, GpuBackend::Other);
    assert_eq!(facts.device_type, GpuType::Other);
    assert!(!facts.unified_memory);
    assert_eq!(facts.driver_version, None);
    let virtual_gpu = normalize(&adapter(
        "Virtio GPU",
        0x1AF4,
        0x1050,
        "VirtualGpu",
        "Vulkan",
        "",
    ));
    assert_eq!(virtual_gpu.device_type, GpuType::Virtual);
    assert!(!virtual_gpu.unified_memory);
}

#[test]
fn unknown_driver_strings_are_omitted() {
    for none in [
        "",
        "unknown",
        "Apple",
        "NVIDIA 560.35.03",
        "Mesa",
        "Mesa unknown",
        "560.35.03abc",
        "560..35",
        "24.",
        ".5",
        "-1",
        "99999999999",
        "release 1.2",
        "AMD proprietary driver",
    ] {
        assert_eq!(parse_driver_version(none), None, "{none:?}");
    }
    let parts = |text| parse_driver_version(text).map(|v| v.parts().to_vec());
    assert_eq!(parts("31.0.15.3640"), Some(vec![31, 0, 15, 3640]));
    assert_eq!(parts("1.2.3.4.5"), Some(vec![1, 2, 3, 4]));
    assert_eq!(parts("  550 "), Some(vec![550]));
    assert_eq!(parts("Mesa 25.1.0-devel (git-abc)"), Some(vec![25, 1, 0]));
    assert_eq!(parts("4294967295"), Some(vec![u32::MAX]));
}

#[test]
fn apple_names_parse_strictly() {
    assert_eq!(
        parse_apple_name("Apple M4 Pro"),
        Some((4, Some(AppleTier::Pro)))
    );
    assert_eq!(
        parse_apple_name("Apple M3 Max"),
        Some((3, Some(AppleTier::Max)))
    );
    assert_eq!(
        parse_apple_name("Apple M2"),
        Some((2, Some(AppleTier::Base)))
    );
    assert_eq!(parse_apple_name("Apple M5 Extreme"), Some((5, None)));
    for none in [
        "",
        "Apple",
        "Apple M",
        "Apple M0",
        "Apple M65",
        "Apple A15 GPU",
        "AMD Radeon",
        "Apple Mx",
    ] {
        assert_eq!(parse_apple_name(none), None, "{none:?}");
    }
}
