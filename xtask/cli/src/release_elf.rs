//! What a Linux release's executables import (`docs/design/release-v0.1.md#candidate-records`):
//! each one's `NEEDED` libraries and the symbol versions it requires of them, read with GNU
//! `readelf` from the built binaries, and the refusal of a build that needs a newer glibc than the
//! Ubuntu 24.04 baseline or links a library the documented runtime requirements do not name.
use crate::{Result, ensure};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

/// The glibc of the Linux baseline, Ubuntu 24.04 (`docs/engineering/platforms.md`).
pub const GLIBC_BASELINE: &str = "2.39";
/// The libraries a release executable may link: glibc's, the GCC runtime and the C++ runtime, each
/// part of every Ubuntu 24.04 installation. Anything else is a new runtime requirement, which the
/// packaged help and `docs/engineering/platforms.md` must name before a build may link it.
pub const SYSTEM_LIBRARIES: &[&str] = &[
    "ld-linux-x86-64.so.2",
    "libc.so.6",
    "libm.so.6",
    "libdl.so.2",
    "libpthread.so.0",
    "librt.so.1",
    "libgcc_s.so.1",
    "libstdc++.so.6",
];

/// One executable's imports.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Imports {
    /// The `NEEDED` entries of its dynamic section, in order.
    pub needed: Vec<String>,
    /// The versions it requires of each library, from `.gnu.version_r`.
    pub versions: BTreeMap<String, BTreeSet<String>>,
}

impl Imports {
    /// Reads `readelf -W --dynamic --version-info` output.
    pub fn parse(text: &str) -> Result<Imports> {
        let mut imports = Imports::default();
        let mut file: Option<String> = None;
        let mut in_needs = false;
        for line in text.lines() {
            if let Some(rest) = line.split_once("(NEEDED)").map(|(_, rest)| rest) {
                let library = rest
                    .trim()
                    .strip_prefix("Shared library: [")
                    .and_then(|rest| rest.strip_suffix(']'))
                    .ok_or_else(|| format!("Unreadable NEEDED entry: {line}"))?;
                imports.needed.push(library.to_owned());
                continue;
            }
            if line.starts_with("Version needs section") {
                in_needs = true;
                continue;
            }
            if line.trim().is_empty() || line.starts_with("Version ") {
                in_needs = false;
                file = None;
                continue;
            }
            if !in_needs {
                continue;
            }
            let fields: Vec<&str> = line.split_whitespace().collect();
            if let Some(at) = fields.iter().position(|field| *field == "File:") {
                file = Some(
                    fields
                        .get(at + 1)
                        .ok_or_else(|| format!("Unreadable version need: {line}"))?
                        .to_string(),
                );
            } else if let Some(at) = fields.iter().position(|field| *field == "Name:") {
                let library = file
                    .clone()
                    .ok_or_else(|| format!("A version need names no library: {line}"))?;
                let version = fields
                    .get(at + 1)
                    .ok_or_else(|| format!("Unreadable version need: {line}"))?;
                imports
                    .versions
                    .entry(library)
                    .or_default()
                    .insert((*version).to_owned());
            }
        }
        ensure(
            !imports.needed.is_empty(),
            "readelf reported no NEEDED libraries: not a dynamically linked executable",
        )?;
        Ok(imports)
    }

    /// The newest `GLIBC_x.y[.z]` version required of any library.
    pub fn max_glibc(&self) -> Option<Vec<u32>> {
        self.versions
            .values()
            .flatten()
            .filter_map(|version| glibc_version(version))
            .max()
    }
}

/// `GLIBC_2.2.5` as `[2, 2, 5]`; `None` for any other version name, such as `GLIBC_PRIVATE`.
pub fn glibc_version(name: &str) -> Option<Vec<u32>> {
    name.strip_prefix("GLIBC_")?
        .split('.')
        .map(|part| part.parse().ok())
        .collect()
}

fn dotted(version: &[u32]) -> String {
    version
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(".")
}

/// The record of every executable's imports, as `build.json` and `target.json` hold it under
/// `elf_imports`, after refusing any executable that requires a glibc newer than
/// [`GLIBC_BASELINE`], links a library outside [`SYSTEM_LIBRARIES`] or requires a private glibc
/// symbol.
pub fn record(readelf: &str, executables: &[(&str, Imports)]) -> Result<Value> {
    let baseline = glibc_version(&format!("GLIBC_{GLIBC_BASELINE}")).ok_or("Baseline")?;
    let mut overall: Option<Vec<u32>> = None;
    let mut entries = Map::new();
    for (name, imports) in executables {
        for library in &imports.needed {
            ensure(
                SYSTEM_LIBRARIES.contains(&library.as_str()),
                format!(
                    "{name} links {library}, which the Linux runtime requirements do not name; \
                     document it in the packaged help and docs/engineering/platforms.md and add \
                     it to SYSTEM_LIBRARIES, or stop linking it"
                ),
            )?;
        }
        ensure(
            !imports
                .versions
                .values()
                .flatten()
                .any(|version| version == "GLIBC_PRIVATE"),
            format!("{name} requires private glibc symbols"),
        )?;
        let max = imports.max_glibc();
        if let Some(max) = &max {
            ensure(
                *max <= baseline,
                format!(
                    "{name} requires GLIBC_{}, newer than the Ubuntu 24.04 baseline's {GLIBC_BASELINE}; \
                     build on the baseline",
                    dotted(max)
                ),
            )?;
        }
        if max > overall {
            overall.clone_from(&max);
        }
        let versions: Map<String, Value> = imports
            .versions
            .iter()
            .map(|(library, versions)| (library.clone(), json!(versions)))
            .collect();
        entries.insert(
            (*name).to_owned(),
            json!({
                "needed": imports.needed,
                "versions": versions,
                "max_glibc": max.as_deref().map(dotted),
            }),
        );
    }
    Ok(json!({
        "inspected_with": readelf,
        "glibc_baseline": GLIBC_BASELINE,
        "max_glibc": overall.as_deref().map(dotted),
        "executables": entries,
    }))
}

/// Checks a Linux record's `elf_imports`: inspected, against [`GLIBC_BASELINE`], with every
/// executable within it.
pub fn check_record(value: &Value, executables: &[&str]) -> Result {
    let baseline = glibc_version(&format!("GLIBC_{GLIBC_BASELINE}")).ok_or("Baseline")?;
    let within = |max: &Value| {
        max.as_str()
            .and_then(|max| glibc_version(&format!("GLIBC_{max}")))
            .is_some_and(|max| max <= baseline)
    };
    ensure(
        value["glibc_baseline"] == GLIBC_BASELINE
            && value["inspected_with"]
                .as_str()
                .is_some_and(|tool| !tool.trim().is_empty())
            && within(&value["max_glibc"]),
        "A Linux build record has no ELF import inspection within the glibc baseline",
    )?;
    let recorded = value["executables"]
        .as_object()
        .ok_or("A Linux build record's ELF inspection names no executables")?;
    ensure(
        recorded.len() == executables.len()
            && executables.iter().all(|name| {
                recorded.get(*name).is_some_and(|entry| {
                    entry["needed"].as_array().is_some_and(|n| !n.is_empty())
                        && within(&entry["max_glibc"])
                })
            }),
        "A Linux build record's ELF inspection does not cover exactly its executables",
    )
}

/// Inspects each `(name, path)` with the build host's GNU `readelf` and returns the checked record.
pub fn inspect(executables: &[(&str, &Path)]) -> Result<Value> {
    let version = Command::new("readelf")
        .arg("--version")
        .output()
        .map_err(|error| format!("readelf is required to inspect a Linux build: {error}"))?;
    ensure(version.status.success(), "readelf --version failed")?;
    let tool = String::from_utf8_lossy(&version.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned();
    let mut inspected = Vec::new();
    for (name, path) in executables {
        let result = Command::new("readelf")
            .args(["-W", "--dynamic", "--version-info"])
            .arg(path)
            .env("LC_ALL", "C")
            .output()?;
        ensure(
            result.status.success(),
            format!(
                "readelf could not read {}: {}",
                path.display(),
                String::from_utf8_lossy(&result.stderr).trim()
            ),
        )?;
        inspected.push((*name, Imports::parse(&String::from_utf8(result.stdout)?)?));
    }
    record(&format!("{tool} (-W --dynamic --version-info)"), &inspected)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GNU readelf 2.44's output for a development editor the hosted Ubuntu 24.04 runner built,
    /// abridged.
    const EDITOR: &str = "
Dynamic section at offset 0x11a1d20 contains 30 entries:
  Tag        Type                         Name/Value
 0x0000000000000001 (NEEDED)             Shared library: [libgcc_s.so.1]
 0x0000000000000001 (NEEDED)             Shared library: [libm.so.6]
 0x0000000000000001 (NEEDED)             Shared library: [libc.so.6]
 0x0000000000000001 (NEEDED)             Shared library: [ld-linux-x86-64.so.2]
 0x000000000000000c (INIT)               0x1106ee8

Version symbols section '.gnu.version' contains 186 entries:
 Addr: 0x00000000000014b0  Offset: 0x000014b0  Link: 4 (.dynsym)
  000:   0 (*local*)       2 (GLIBC_2.34)    1 (*global*)      1 (*global*)
  004:   1 (*global*)      3 (GLIBC_2.2.5)   4 (GLIBC_2.14)    5 (GCC_3.0)

Version needs section '.gnu.version_r' contains 4 entries:
 Addr: 0x0000000000001624  Offset: 0x00001624  Link: 8 (.dynstr)
  000000: Version: 1  File: libgcc_s.so.1  Cnt: 3
  0x0040:   Name: GCC_3.0  Flags: none  Version: 5
  0x0050:   Name: GCC_3.3  Flags: none  Version: 25
  0x0060:   Name: GCC_4.2.0  Flags: none  Version: 26
  0x0010: Version: 1  File: libm.so.6  Cnt: 4
  0x0070:   Name: GLIBC_2.2.5  Flags: none  Version: 6
  0x0080:   Name: GLIBC_2.27  Flags: none  Version: 9
  0x0090:   Name: GLIBC_2.29  Flags: none  Version: 12
  0x00a0:   Name: GLIBC_2.35  Flags: none  Version: 11
  0x0020: Version: 1  File: libc.so.6  Cnt: 4
  0x00b0:   Name: GLIBC_2.2.5  Flags: none  Version: 3
  0x00c0:   Name: GLIBC_2.3.4  Flags: none  Version: 8
  0x01a0:   Name: GLIBC_2.34  Flags: none  Version: 2
  0x01b0:   Name: GLIBC_2.39  Flags: none  Version: 20
  0x0030: Version: 1  File: ld-linux-x86-64.so.2  Cnt: 1
  0x01c0:   Name: GLIBC_2.3  Flags: none  Version: 7
";

    #[test]
    fn readelf_output_gives_needed_libraries_and_required_versions() {
        let imports = Imports::parse(EDITOR).unwrap();
        assert_eq!(
            imports.needed,
            [
                "libgcc_s.so.1",
                "libm.so.6",
                "libc.so.6",
                "ld-linux-x86-64.so.2"
            ]
        );
        assert_eq!(imports.versions.len(), 4);
        assert!(imports.versions["libgcc_s.so.1"].contains("GCC_4.2.0"));
        assert!(imports.versions["libm.so.6"].contains("GLIBC_2.35"));
        assert_eq!(imports.versions["ld-linux-x86-64.so.2"].len(), 1);
        // The `.gnu.version` symbol table names versions too, but not what is required of whom.
        assert_eq!(imports.versions["libc.so.6"].len(), 4);
        assert_eq!(imports.max_glibc(), Some(vec![2, 39]));
        assert_eq!(glibc_version("GLIBC_2.2.5"), Some(vec![2, 2, 5]));
        assert_eq!(glibc_version("GLIBC_PRIVATE"), None);
        assert_eq!(glibc_version("GCC_3.0"), None);
        // A static executable, or text that is not readelf's, is refused.
        assert!(Imports::parse("There is no dynamic section in this file.\n").is_err());
    }

    #[test]
    fn the_record_holds_imports_and_refuses_a_build_past_the_baseline() {
        let tool = "GNU readelf (GNU Binutils for Ubuntu) 2.42 (-W --dynamic --version-info)";
        let editor = || Imports::parse(EDITOR).unwrap();
        let record = record(tool, &[("luxforge", editor()), ("luxforge-ctl", editor())]).unwrap();
        assert_eq!(record["glibc_baseline"], "2.39");
        assert_eq!(record["max_glibc"], "2.39");
        assert_eq!(record["inspected_with"], tool);
        assert_eq!(record["executables"]["luxforge"]["needed"][2], "libc.so.6");
        assert_eq!(
            record["executables"]["luxforge-ctl"]["versions"]["libm.so.6"][3],
            "GLIBC_2.35"
        );
        check_record(&record, &["luxforge", "luxforge-ctl"]).unwrap();
        assert!(check_record(&record, &["luxforge", "luxforge-ctl", "luxforge-json"]).is_err());

        let newer = EDITOR.replace("GLIBC_2.39", "GLIBC_2.40");
        let error = super::record(tool, &[("luxforge", Imports::parse(&newer).unwrap())])
            .unwrap_err()
            .to_string();
        assert!(error.contains("GLIBC_2.40"), "{error}");
        // 2.4 is older than 2.39: versions compare numerically.
        let older = EDITOR.replace("GLIBC_2.39", "GLIBC_2.4");
        let record = super::record(tool, &[("luxforge", Imports::parse(&older).unwrap())]).unwrap();
        assert_eq!(record["max_glibc"], "2.35");

        let other = EDITOR.replace("[libgcc_s.so.1]", "[libssl.so.3]");
        let error = super::record(tool, &[("luxforge", Imports::parse(&other).unwrap())])
            .unwrap_err()
            .to_string();
        assert!(error.contains("libssl.so.3"), "{error}");
        let private = EDITOR.replace("GLIBC_2.3.4", "GLIBC_PRIVATE");
        assert!(super::record(tool, &[("luxforge", Imports::parse(&private).unwrap())]).is_err());

        let mut beyond = super::record(tool, &[("luxforge", editor())]).unwrap();
        beyond["max_glibc"] = json!("2.41");
        assert!(check_record(&beyond, &["luxforge"]).is_err());
        assert!(check_record(&json!(null), &["luxforge"]).is_err());
    }
}
