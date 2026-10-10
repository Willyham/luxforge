use super::*;

fn v(parts: &[u32]) -> Version {
    Version::new(parts).unwrap()
}

#[test]
fn apple_brands_give_a_generation_and_tier() {
    let tier = |brand| parse_apple_brand(brand).map(|a| (a.generation, a.tier));
    assert_eq!(tier("Apple M1"), Some((1, Some(AppleTier::Base))));
    assert_eq!(tier("Apple M2 Pro"), Some((2, Some(AppleTier::Pro))));
    assert_eq!(tier("Apple M3 Max"), Some((3, Some(AppleTier::Max))));
    assert_eq!(tier("Apple M4 Pro"), Some((4, Some(AppleTier::Pro))));
    assert_eq!(tier("Apple M4 Ultra"), Some((4, Some(AppleTier::Ultra))));
    assert_eq!(tier("Apple M2"), Some((2, Some(AppleTier::Base))));
    // A generation without a recognised tier keeps the generation and omits the tier.
    assert_eq!(tier("Apple M5 Extreme"), Some((5, None)));
    assert_eq!(tier("Apple M5 Pro Max"), Some((5, None)));
    for unknown in [
        "",
        "Apple",
        "Apple A17 Pro",
        "Apple M",
        "Apple Mx",
        "Apple M0",
        "Apple M65",
        "Apple M999999",
        "Intel(R) Core(TM) i9-9980HK CPU @ 2.40GHz",
        "VirtualApple @ 2.50GHz processor (emulated)",
    ] {
        assert_eq!(parse_apple_brand(unknown), None, "{unknown:?}");
    }
}

#[test]
fn x86_signatures_follow_the_cpuid_rules() {
    let sig = |eax| {
        let s = X86Signature::from_cpuid_eax(eax);
        (s.family, s.model, s.stepping)
    };
    // Intel Alder Lake-S, family 6 with extended model 9: model 0x9A.
    assert_eq!(sig(0x0009_06A3), (6, 154, 3));
    // Intel Skylake-S: family 6, model 0x5E, stepping 3.
    assert_eq!(sig(0x0005_06E3), (6, 94, 3));
    // AMD Zen 4: family 0xF plus extended family 0xA is 25, model 0x74.
    assert_eq!(sig(0x00A7_0F41), (25, 116, 1));
    // AMD Zen 2 Matisse: family 23, model 0x71.
    assert_eq!(sig(0x0087_0F10), (23, 113, 0));
    assert_eq!(sig(0x00A2_0F10), (25, 33, 0));
    // A family that is neither 6 nor 15 ignores the extended model.
    assert_eq!(sig(0x000F_0520), (5, 2, 0));
    // The largest values stay inside 0..=4095.
    assert_eq!(sig(0xFFFF_FFFF), (270, 255, 15));
}

#[test]
fn cpuid_vendor_strings_map_to_the_enum() {
    assert_eq!(vendor_from_id("GenuineIntel"), CpuVendor::Intel);
    assert_eq!(vendor_from_id("AuthenticAMD"), CpuVendor::Amd);
    assert_eq!(vendor_from_id("CentaurHauls"), CpuVendor::Other);
    assert_eq!(vendor_from_id(""), CpuVendor::Other);
    assert_eq!(vendor_from_arm_implementer(0x51), CpuVendor::Qualcomm);
    assert_eq!(vendor_from_arm_implementer(0x61), CpuVendor::Apple);
    assert_eq!(vendor_from_arm_implementer(0x41), CpuVendor::Other);
}

#[test]
fn os_release_maps_to_the_fixed_distro_enum() {
    let ubuntu = "NAME=\"Ubuntu\"\nVERSION=\"24.04.1 LTS (Noble Numbat)\"\nID=ubuntu\n\
                  ID_LIKE=debian\nPRETTY_NAME=\"Ubuntu 24.04.1 LTS\"\nVERSION_ID=\"24.04\"\n\
                  HOME_URL=\"https://www.ubuntu.com/\"\n";
    let parsed = parse_os_release(ubuntu);
    assert_eq!(parsed.distro, LinuxDistro::Ubuntu);
    assert_eq!(parsed.version, Some(v(&[24, 4])));

    // Arch publishes no VERSION_ID.
    let arch = "NAME=\"Arch Linux\"\nPRETTY_NAME=\"Arch Linux\"\nID=arch\nBUILD_ID=rolling\n";
    assert_eq!(
        parse_os_release(arch),
        OsRelease {
            distro: LinuxDistro::Arch,
            version: None
        }
    );

    let fedora = "NAME=\"Fedora Linux\"\nID=fedora\nVERSION_ID=41\n";
    assert_eq!(
        parse_os_release(fedora),
        OsRelease {
            distro: LinuxDistro::Fedora,
            version: Some(v(&[41]))
        }
    );

    // A derivative maps through ID_LIKE and does not borrow its own release as its parent's.
    let mint = "ID=linuxmint\nID_LIKE=\"ubuntu debian\"\nVERSION_ID=\"22\"\n";
    assert_eq!(
        parse_os_release(mint),
        OsRelease {
            distro: LinuxDistro::Ubuntu,
            version: None
        }
    );
    let rocky = "ID=\"rocky\"\nID_LIKE=\"rhel centos fedora\"\nVERSION_ID=\"9.4\"\n";
    assert_eq!(parse_os_release(rocky).distro, LinuxDistro::Rhel);

    for (id, distro) in [
        ("opensuse-tumbleweed", LinuxDistro::Opensuse),
        ("opensuse-leap", LinuxDistro::Opensuse),
        ("nixos", LinuxDistro::Nixos),
        ("alpine", LinuxDistro::Alpine),
        ("debian", LinuxDistro::Debian),
        ("rhel", LinuxDistro::Rhel),
    ] {
        assert_eq!(
            parse_os_release(&format!("ID={id}\n")).distro,
            distro,
            "{id}"
        );
    }

    let unknown = "ID=\"my-secret-distro\"\nPRETTY_NAME=\"Mine\"\nVERSION_ID=\"3.1.4\"\n";
    assert_eq!(
        parse_os_release(unknown),
        OsRelease {
            distro: LinuxDistro::Other,
            version: None
        }
    );
    assert_eq!(parse_os_release("").distro, LinuxDistro::Other);
}

#[test]
fn versions_keep_numeric_components_and_stop_at_a_suffix() {
    assert_eq!(parse_version("6.8.0-45-generic"), Some(v(&[6, 8, 0])));
    assert_eq!(parse_version("7.2.6-arch1-1"), Some(v(&[7, 2, 6])));
    assert_eq!(parse_version("6.1.0\n"), Some(v(&[6, 1, 0])));
    assert_eq!(
        parse_version("5.15.153.1-microsoft-standard-WSL2"),
        Some(v(&[5, 15, 153, 1]))
    );
    assert_eq!(parse_version("26.0.1"), Some(v(&[26, 0, 1])));
    assert_eq!(parse_version("15.5"), Some(v(&[15, 5])));
    assert_eq!(parse_version("1.2.3.4.5"), Some(v(&[1, 2, 3, 4])));
    assert_eq!(parse_version("4294967295"), Some(v(&[4_294_967_295])));
    // A component that overflows u32 ends the version.
    assert_eq!(parse_version("1.4294967296"), Some(v(&[1])));
    assert_eq!(parse_version("4294967296"), None);
    for none in ["", "x.y", "-1.2", ".5", "rolling"] {
        assert_eq!(parse_version(none), None, "{none:?}");
    }
    assert_eq!(Version::new(&[]), None);
    assert_eq!(Version::new(&[1, 2, 3, 4, 5]), None);
}

#[test]
fn mac_builds_must_have_the_exact_shape() {
    for ok in ["24G84", "25A354", "9A1", "23H124", "21A5522h", "999Z99999z"] {
        assert_eq!(
            MacOsBuild::parse(ok).as_ref().map(MacOsBuild::as_str),
            Some(ok)
        );
    }
    for bad in [
        "",
        "24",
        "G84",
        "24g84",
        "1000A1",
        "24G",
        "24G123456",
        "24G84a1",
        "24G84ab",
        "24G84H",
        "24G84 ",
        " 24G84",
        "24-G84",
        "Darwin Kernel",
        "24G84\n",
    ] {
        assert_eq!(MacOsBuild::parse(bad), None, "{bad:?}");
    }
}

#[test]
fn memory_bands_hold_their_edges() {
    const GIB: u64 = 1 << 30;
    let band = MemoryBand::of_bytes;
    assert_eq!(band(0), None);
    assert_eq!(band(1), Some(MemoryBand::Gib8));
    assert_eq!(band(4 * GIB), Some(MemoryBand::Gib8));
    assert_eq!(band(8 * GIB), Some(MemoryBand::Gib8));
    assert_eq!(band(8 * GIB + 1), Some(MemoryBand::Gib16));
    // The OS reports a little under what is installed.
    assert_eq!(band(16 * GIB - 300 * (1 << 20)), Some(MemoryBand::Gib16));
    assert_eq!(band(16 * GIB), Some(MemoryBand::Gib16));
    assert_eq!(band(16 * GIB + 1), Some(MemoryBand::Gib32));
    assert_eq!(band(24 * GIB), Some(MemoryBand::Gib32));
    assert_eq!(band(48 * GIB), Some(MemoryBand::Gib64));
    assert_eq!(band(64 * GIB), Some(MemoryBand::Gib64));
    assert_eq!(band(96 * GIB), Some(MemoryBand::Gib128));
    assert_eq!(band(192 * GIB), Some(MemoryBand::Gib256));
    assert_eq!(band(512 * GIB), Some(MemoryBand::Gib512));
    assert_eq!(band(1024 * GIB), Some(MemoryBand::Gib1024));
    assert_eq!(band(1024 * GIB + 1), Some(MemoryBand::Over1024));
    assert_eq!(band(u64::MAX), Some(MemoryBand::Over1024));
    assert_eq!(MemoryBand::Gib8.upper_gib(), Some(8));
    assert_eq!(MemoryBand::Gib1024.upper_gib(), Some(1024));
    assert_eq!(MemoryBand::Over1024.upper_gib(), None);
}

#[test]
fn core_counts_are_bounded() {
    assert_eq!(core_count(0), None);
    assert_eq!(core_count(1), Some(1));
    assert_eq!(core_count(1024), Some(1024));
    assert_eq!(core_count(1025), None);
    assert_eq!(core_count(u64::MAX), None);
}

#[test]
fn cpuinfo_gives_counts_and_the_arm_implementer_only() {
    let x86 = "processor\t: 0\nvendor_id\t: GenuineIntel\ncpu family\t: 6\nmodel name\t: Secret CPU\n\
               physical id\t: 0\ncore id\t: 0\n\nprocessor\t: 1\nphysical id\t: 0\ncore id\t: 0\n\n\
               processor\t: 2\nphysical id\t: 0\ncore id\t: 1\n\nprocessor\t: 3\nphysical id\t: 0\n\
               core id\t: 1\n";
    assert_eq!(
        parse_cpuinfo(x86),
        CpuInfo {
            vendor: None,
            logical: Some(4),
            physical: Some(2)
        }
    );
    let arm = "processor\t: 0\nBogoMIPS\t: 50.00\nCPU implementer\t: 0x51\nCPU part\t: 0x001\n\n\
               processor\t: 1\nCPU implementer\t: 0x41\n";
    assert_eq!(
        parse_cpuinfo(arm),
        CpuInfo {
            vendor: Some(CpuVendor::Qualcomm),
            logical: Some(2),
            physical: None
        }
    );
    assert_eq!(parse_cpuinfo(""), CpuInfo::default());
}

#[test]
fn meminfo_gives_installed_bytes() {
    let meminfo = "MemTotal:       16318464 kB\nMemFree:         1000 kB\n";
    assert_eq!(parse_mem_total(meminfo), Some(16_318_464 * 1024));
    assert_eq!(parse_mem_total("MemFree: 1 kB\n"), None);
    assert_eq!(parse_mem_total("MemTotal: lots kB\n"), None);
    assert_eq!(parse_mem_total("MemTotal: 5\n"), None);
    assert_eq!(
        MemoryBand::of_bytes(parse_mem_total(meminfo).unwrap()),
        Some(MemoryBand::Gib16)
    );
}

#[test]
fn a_linux_system_is_normalized_without_its_strings() {
    let raw = Raw {
        os_release: Some("ID=ubuntu\nVERSION_ID=\"24.04\"\nPRETTY_NAME=\"private\"\n".into()),
        kernel_release: Some("6.8.0-45-generic\n".into()),
        cpu_vendor: Some(CpuVendor::Amd),
        x86: Some(X86Signature {
            family: 25,
            model: 116,
            stepping: 1,
        }),
        physical_cores: Some(8),
        logical_cores: Some(16),
        memory_bytes: Some(31 << 30),
        mac_build: Some("24G84".into()),
        cpu_brand: Some("AMD Ryzen 7 7840U".into()),
        ..Raw::default()
    };
    let facts = normalize(OsFamily::Linux, raw);
    assert_eq!(facts.os.family, OsFamily::Linux);
    assert_eq!(facts.os.version, Some(v(&[6, 8, 0])));
    assert_eq!(facts.os.kernel, Some(v(&[6, 8, 0])));
    assert_eq!(facts.os.distro, Some(LinuxDistro::Ubuntu));
    assert_eq!(facts.os.distro_version, Some(v(&[24, 4])));
    // The macOS build is for macOS only.
    assert_eq!(facts.os.build, None);
    assert_eq!(facts.cpu.vendor, CpuVendor::Amd);
    assert_eq!(facts.cpu.x86.map(|s| s.family), Some(25));
    assert_eq!(facts.cpu.apple_generation, None);
    assert_eq!(
        (facts.cpu.physical_cores, facts.cpu.logical_cores),
        (Some(8), Some(16))
    );
    assert_eq!(facts.memory, Some(MemoryBand::Gib32));
}

#[test]
fn a_macos_system_is_normalized_and_bad_values_are_omitted() {
    let raw = Raw {
        os_version: Some(v(&[15, 5])),
        mac_build: Some("24F74".into()),
        os_release: Some("ID=ubuntu\n".into()),
        kernel_release: Some("24.5.0".into()),
        cpu_brand: Some("Apple M4 Pro".into()),
        physical_cores: Some(14),
        logical_cores: Some(14),
        memory_bytes: Some(48 << 30),
        ..Raw::default()
    };
    let facts = normalize(OsFamily::Macos, raw);
    assert_eq!(facts.os.version, Some(v(&[15, 5])));
    assert_eq!(
        facts.os.build.as_ref().map(MacOsBuild::as_str),
        Some("24F74")
    );
    assert_eq!(
        (facts.os.distro, facts.os.distro_version, facts.os.kernel),
        (None, None, None)
    );
    assert_eq!(facts.cpu.vendor, CpuVendor::Apple);
    assert_eq!(facts.cpu.apple_generation, Some(4));
    assert_eq!(facts.cpu.apple_tier, Some(AppleTier::Pro));
    assert_eq!(facts.cpu.x86, None);
    assert_eq!(facts.memory, Some(MemoryBand::Gib64));

    let odd = normalize(
        OsFamily::Macos,
        Raw {
            mac_build: Some("not a build".into()),
            cpu_brand: Some("Apple Mystery".into()),
            physical_cores: Some(0),
            logical_cores: Some(5000),
            memory_bytes: Some(0),
            ..Raw::default()
        },
    );
    assert_eq!(odd.os.build, None);
    assert_eq!(odd.cpu.vendor, CpuVendor::Other);
    assert_eq!((odd.cpu.apple_generation, odd.cpu.apple_tier), (None, None));
    assert_eq!(
        (odd.cpu.physical_cores, odd.cpu.logical_cores),
        (None, None)
    );
    assert_eq!(odd.memory, None);
}

#[test]
fn an_unrecognised_vendor_never_keeps_an_x86_signature() {
    let raw = Raw {
        cpu_vendor: Some(CpuVendor::Other),
        x86: Some(X86Signature {
            family: 6,
            model: 1,
            stepping: 1,
        }),
        ..Raw::default()
    };
    assert_eq!(normalize(OsFamily::Other, raw).cpu.x86, None);
}

/// The live probe on this host: plausible, not exact.
#[test]
fn the_probe_runs_on_this_host() {
    let facts = probe();
    assert!(facts.cpu.logical_cores.is_some_and(|cores| cores >= 1));
    assert!(facts.cpu.physical_cores.is_some_and(|cores| cores >= 1));
    assert!(facts.memory.is_some());
    assert_eq!(facts.arch, Arch::current());
    if cfg!(target_os = "macos") {
        assert_eq!(facts.os.family, OsFamily::Macos);
        assert!(
            facts
                .os
                .version
                .is_some_and(|version| version.parts()[0] >= 10)
        );
        assert!(facts.os.build.is_some());
        assert_eq!(facts.os.distro, None);
    }
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        assert_eq!(facts.cpu.vendor, CpuVendor::Apple);
        assert!(facts.cpu.apple_generation.is_some());
    }
    if cfg!(target_os = "linux") {
        assert_eq!(facts.os.family, OsFamily::Linux);
        assert!(facts.os.kernel.is_some());
    }
    if cfg!(windows) {
        assert_eq!(facts.os.family, OsFamily::Windows);
    }
    eprintln!("system facts: {facts:?}");
}
