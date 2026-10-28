//! What Wasmtime costs the editor's binary and its build. "Wasmtime" here is every package only
//! `luxforge-wasm` brings into the editor's dependency graph (`luxforge-app`'s normal and build
//! dependencies on this host's platform), and `luxforge-wasm` itself: the packages the graph
//! loses when the edges into `luxforge-wasm` are cut ([`exclusive`]).
//!
//! - **size** attributes the release binary's text symbols to crates by their demangled paths,
//!   each symbol's size the distance to the next text symbol's address (as `nm -n` lists them on
//!   both macOS and Linux): `leading`, a symbol whose path starts in one of those crates, and
//!   `mentioning`, one that names any of them anywhere, such as a generic instantiated for one of
//!   their types. An estimate by symbols, not a build without them.
//! - **build** builds `luxforge-wasm` alone in release in a new target directory with no compiler
//!   wrapper and no incremental compilation, `--timings`, and sums Cargo's own unit durations of
//!   those packages; the target directory is removed after, its timings page kept.
use super::{Report, milliseconds};
use crate::*;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::time::Instant;

const APP: &str = "luxforge-app";
const WASM: &str = "luxforge-wasm";

/// One package of the graph: its name, version and library crate name, when it has one.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Package {
    pub name: String,
    pub version: String,
    pub crate_name: Option<String>,
}

/// The packages in `from`'s normal and build closure that the closure loses when every edge into
/// `cut` is removed, `cut` among them, from `cargo metadata`'s resolve graph.
pub(crate) fn exclusive(metadata: &Value, from: &str, cut: &str) -> Result<Vec<Package>> {
    let packages: HashMap<&str, &Value> = metadata["packages"]
        .as_array()
        .ok_or("cargo metadata listed no packages")?
        .iter()
        .filter_map(|package| Some((package["id"].as_str()?, package)))
        .collect();
    let named = |name: &str| {
        packages
            .iter()
            .find(|(_, package)| package["name"] == name && package["source"].is_null())
            .map(|(id, _)| *id)
            .ok_or_else(|| format!("cargo metadata has no workspace package {name}"))
    };
    let (from, cut) = (named(from)?, named(cut)?);
    let mut edges: HashMap<&str, Vec<&str>> = HashMap::new();
    for node in metadata["resolve"]["nodes"]
        .as_array()
        .ok_or("cargo metadata has no resolve graph")?
    {
        let id = node["id"].as_str().ok_or("a node without an id")?;
        let deps = node["deps"].as_array().map_or(&[][..], Vec::as_slice);
        edges.insert(
            id,
            deps.iter()
                .filter(|dep| {
                    dep["dep_kinds"].as_array().is_some_and(|kinds| {
                        kinds
                            .iter()
                            .any(|kind| kind["kind"].is_null() || kind["kind"] == "build")
                    })
                })
                .filter_map(|dep| dep["pkg"].as_str())
                .collect(),
        );
    }
    let closure = |skip: Option<&str>| {
        let mut seen = HashSet::new();
        let mut stack = vec![from];
        while let Some(id) = stack.pop() {
            if Some(id) == skip || !seen.insert(id) {
                continue;
            }
            stack.extend(edges.get(id).into_iter().flatten().copied());
        }
        seen
    };
    let (with, without) = (closure(None), closure(Some(cut)));
    let mut lost: Vec<Package> = with
        .difference(&without)
        .filter_map(|id| packages.get(id))
        .map(|package| Package {
            name: package["name"].as_str().unwrap_or_default().to_owned(),
            version: package["version"].as_str().unwrap_or_default().to_owned(),
            crate_name: package["targets"].as_array().and_then(|targets| {
                targets.iter().find_map(|target| {
                    let kinds = target["kind"].as_array()?;
                    kinds
                        .iter()
                        .any(|kind| kind == "lib" || kind == "rlib")
                        .then(|| target["name"].as_str().map(|n| n.replace('-', "_")))?
                })
            }),
        })
        .collect();
    lost.sort();
    Ok(lost)
}

/// `cargo metadata` for this host's platform.
fn metadata(root: &Path) -> Result<Value> {
    let host = output(root, "rustc", &["-vV"])?
        .lines()
        .find_map(|line| line.strip_prefix("host: ").map(str::to_owned))
        .ok_or("rustc named no host")?;
    Ok(serde_json::from_str(&output(
        root,
        "cargo",
        &[
            "metadata",
            "--locked",
            "--format-version",
            "1",
            "--filter-platform",
            &host,
        ],
    )?)?)
}

/// The text symbols of `nm -n`'s listing, by address, each sized to the next text symbol.
pub(crate) fn text_symbols(listing: &str) -> Vec<(u64, String)> {
    let mut symbols: Vec<(u64, String)> = listing
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let address = u64::from_str_radix(fields.next()?, 16).ok()?;
            let kind = fields.next()?;
            let name = fields.next()?;
            matches!(kind, "t" | "T").then(|| (address, name.to_owned()))
        })
        .collect();
    symbols.sort_by_key(|(address, _)| *address);
    let mut sized = Vec::with_capacity(symbols.len());
    for (index, (address, name)) in symbols.iter().enumerate() {
        let next = symbols[index + 1..]
            .iter()
            .find(|(other, _)| other > address)
            .map(|(other, _)| *other);
        // Aliases share an address: the first carries the size, the rest none; the last symbol
        // has no next to measure to and is left out.
        let first = index == 0 || symbols[index - 1].0 != *address;
        if let (Some(next), true) = (next, first) {
            sized.push((next - address, name.clone()));
        }
    }
    sized
}

/// The crates a demangled path names: every path's first segment.
pub(crate) fn path_heads(demangled: &str) -> Vec<&str> {
    demangled
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
        .filter_map(|token| token.split_once("::").map(|(head, _)| head))
        .filter(|head| !head.is_empty())
        .collect()
}

/// How many text bytes the symbols attribute to `crates`: by their leading path, and by any path
/// they name; and the leading bytes per crate.
pub(crate) fn attribute(
    symbols: &[(u64, String)],
    crates: &BTreeSet<String>,
) -> (u64, u64, u64, BTreeMap<String, u64>) {
    let (mut total, mut leading, mut mentioning) = (0, 0, 0);
    let mut by_crate = BTreeMap::new();
    for (size, name) in symbols {
        total += size;
        let demangled = rustc_demangle::try_demangle(name)
            .map(|d| format!("{d:#}"))
            .unwrap_or_else(|_| name.clone());
        let heads = path_heads(&demangled);
        if let Some(head) = heads.first().filter(|head| crates.contains(**head)) {
            leading += size;
            *by_crate.entry((*head).to_owned()).or_insert(0) += size;
        }
        if heads.iter().any(|head| crates.contains(*head)) {
            mentioning += size;
        }
    }
    (total, leading, mentioning, by_crate)
}

/// `size`: the editor binary's bytes and its text's attribution to the Wasmtime packages.
pub(super) fn size(report: &mut Report, root: &Path, options: &super::Options) -> Result {
    let start = report.load();
    let lost = exclusive(&metadata(root)?, APP, WASM)?;
    let crates: BTreeSet<String> = lost.iter().filter_map(|p| p.crate_name.clone()).collect();
    let bytes = fs::metadata(&options.binary)?.len();
    let listing = output(
        root,
        "nm",
        &["-n", options.binary.to_str().ok_or("binary path encoding")?],
    )?;
    let symbols = text_symbols(&listing);
    ensure(
        !symbols.is_empty(),
        "nm listed no text symbols: is the binary stripped?",
    )?;
    let (total, leading, mentioning, by_crate) = attribute(&symbols, &crates);
    let load = report.span(start);
    let mib = |bytes: u64| bytes as f64 / 1048576.0;
    let scope = format!(
        "the release editor binary {} ({} text symbols); the {} packages only luxforge-wasm \
         brings in, luxforge-wasm among them; symbol sizes are distances between text symbols' \
         addresses, so padding is included and data is not attributed",
        options.binary.display(),
        symbols.len(),
        lost.len()
    );
    for (metric, value, what) in [
        ("size.binary_mib", bytes, "the binary file"),
        ("size.text_symbols_mib", total, "all text symbols"),
        (
            "size.wasmtime_text_leading_mib",
            leading,
            "text symbols whose path starts in a Wasmtime package",
        ),
        (
            "size.wasmtime_text_mentioning_mib",
            mentioning,
            "text symbols naming a Wasmtime package anywhere in their path",
        ),
    ] {
        report.row(
            metric,
            "MiB",
            vec![mib(value)],
            &format!("{what}; {scope}"),
            &load,
        );
    }
    let mut largest: Vec<(&String, &u64)> = by_crate.iter().collect();
    largest.sort_by(|a, b| b.1.cmp(a.1));
    report.details.insert(
        "size".into(),
        json!({
            "binary_bytes": bytes,
            "packages": lost.iter().map(|p| format!("{} {}", p.name, p.version)).collect::<Vec<_>>(),
            "leading_bytes_by_crate": largest.iter().take(25)
                .map(|(name, bytes)| json!({"crate": name, "bytes": bytes}))
                .collect::<Vec<_>>(),
        }),
    );
    report.write()
}

/// The units of Cargo's timings page (`cargo-timing.html`'s `UNIT_DATA`).
pub(crate) fn timing_units(html: &str) -> Result<Vec<Value>> {
    let at = html
        .find("UNIT_DATA = ")
        .ok_or("the timings page holds no UNIT_DATA")?;
    let rest = &html[at..];
    let open = rest.find('[').ok_or("UNIT_DATA is not a list")?;
    let units = serde_json::Deserializer::from_str(&rest[open..])
        .into_iter::<Value>()
        .next()
        .ok_or("UNIT_DATA is empty")??;
    Ok(units.as_array().cloned().unwrap_or_default())
}

/// The summed duration, count and span (first start to last end) of the units of `packages`.
pub(crate) fn unit_figures(units: &[Value], packages: &[Package]) -> (f64, usize, f64) {
    let wanted: HashSet<(&str, &str)> = packages
        .iter()
        .map(|p| (p.name.as_str(), p.version.as_str()))
        .collect();
    let (mut sum, mut count) = (0.0, 0);
    let (mut first, mut last) = (f64::INFINITY, 0.0_f64);
    for unit in units {
        let (Some(name), Some(version)) = (unit["name"].as_str(), unit["version"].as_str()) else {
            continue;
        };
        if !wanted.contains(&(name, version)) {
            continue;
        }
        let start = unit["start"].as_f64().unwrap_or(0.0);
        let duration = unit["duration"].as_f64().unwrap_or(0.0);
        sum += duration;
        count += 1;
        first = first.min(start);
        last = last.max(start + duration);
    }
    (sum, count, if count == 0 { 0.0 } else { last - first })
}

/// `build`: a clean, uncached release build of `luxforge-wasm` alone, with Cargo's timings.
pub(super) fn build(report: &mut Report, root: &Path, out: &Path) -> Result {
    let start = report.load();
    let lost = exclusive(&metadata(root)?, APP, WASM)?;
    let target = out.join("build-target");
    let started = Instant::now();
    let status = cargo_command()
        .current_dir(root)
        .args(["build", "--release", "--locked", "-p", WASM, "--timings"])
        .arg("--target-dir")
        .arg(&target)
        .env("RUSTC_WRAPPER", "")
        .env("CARGO_BUILD_RUSTC_WRAPPER", "")
        .env("CARGO_INCREMENTAL", "0")
        .env_remove("CARGO_TARGET_DIR")
        .status()?;
    let wall = milliseconds(started) / 1000.0;
    ensure(
        status.success(),
        format!("the clean build failed: {status}"),
    )?;
    let page = target.join("cargo-timings").join("cargo-timing.html");
    let html = fs::read_to_string(&page)?;
    fs::write(out.join("build-cargo-timing.html"), &html)?;
    fs::remove_dir_all(&target)?;
    let units = timing_units(&html)?;
    let (sum, count, span) = unit_figures(&units, &lost);
    let wasm: Vec<Package> = lost.iter().filter(|p| p.name == WASM).cloned().collect();
    let (own, _, _) = unit_figures(&units, &wasm);
    let all_sum: f64 = units
        .iter()
        .filter_map(|unit| unit["duration"].as_f64())
        .sum();
    let load = report.span(start);
    let scope = format!(
        "`cargo build --release --locked -p luxforge-wasm --timings` in a new target directory, \
         RUSTC_WRAPPER empty and CARGO_INCREMENTAL=0, all {} jobs; Cargo's unit durations of \
         the {} packages only luxforge-wasm brings into the editor ({count} units, build scripts \
         included)",
        std::thread::available_parallelism().map_or(0, usize::from),
        lost.len()
    );
    for (metric, value, what) in [
        (
            "build.clean_wall_s",
            wall,
            "the whole build's wall time, luxforge-core and its dependencies included",
        ),
        (
            "build.all_units_s",
            all_sum,
            "every unit's duration, summed",
        ),
        (
            "build.wasmtime_units_s",
            sum,
            "the Wasmtime packages' unit durations, summed",
        ),
        (
            "build.wasmtime_span_s",
            span,
            "the first Wasmtime unit's start to the last one's end",
        ),
        (
            "build.luxforge_wasm_units_s",
            own,
            "luxforge-wasm's own units",
        ),
    ] {
        report.row(metric, "s", vec![value], &format!("{what}; {scope}"), &load);
    }
    report.details.insert(
        "build".into(),
        json!({"timings": "build-cargo-timing.html", "units": units.len(), "wasmtime_units": count}),
    );
    report.write()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(name: &str, lib: Option<&str>) -> Value {
        json!({"id": format!("{name}-id"), "name": name, "version": "1.0.0",
            "source": if name.starts_with("luxforge") { Value::Null } else { json!("registry") },
            "targets": lib.map_or(json!([{"kind":["bin"],"name":name}]),
                |lib| json!([{"kind":["lib"],"name":lib}]))})
    }

    fn dep(name: &str, kind: Value) -> Value {
        json!({"pkg": format!("{name}-id"), "dep_kinds": [{"kind": kind}]})
    }

    /// Only what the cut edges alone reach is lost: a package the app also reaches another way,
    /// and a dev-dependency, are not.
    #[test]
    fn exclusive_packages_are_those_only_the_cut_package_reaches() {
        let metadata = json!({
            "packages": [package("luxforge-app", None), package("luxforge-wasm", Some("luxforge_wasm")),
                package("wasmtime", Some("wasmtime")), package("cranelift-codegen", Some("cranelift_codegen")),
                package("serde", Some("serde")), package("criterion", Some("criterion")),
                package("cc", Some("cc"))],
            "resolve": {"nodes": [
                {"id": "luxforge-app-id", "deps": [dep("luxforge-wasm", Value::Null), dep("serde", Value::Null)]},
                {"id": "luxforge-wasm-id", "deps": [dep("wasmtime", Value::Null), dep("serde", Value::Null),
                    dep("criterion", json!("dev"))]},
                {"id": "wasmtime-id", "deps": [dep("cranelift-codegen", Value::Null), dep("cc", json!("build"))]},
                {"id": "cranelift-codegen-id", "deps": []},
                {"id": "serde-id", "deps": []}, {"id": "criterion-id", "deps": []}, {"id": "cc-id", "deps": []},
            ]},
        });
        let lost = exclusive(&metadata, APP, WASM).unwrap();
        let names: Vec<&str> = lost.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            ["cc", "cranelift-codegen", "luxforge-wasm", "wasmtime"]
        );
        assert_eq!(lost[1].crate_name.as_deref(), Some("cranelift_codegen"));
    }

    /// Each text symbol is sized to the next address; aliases count once; data and undefined
    /// symbols are left out.
    #[test]
    fn text_symbols_are_sized_by_the_next_address() {
        let listing = "                 U _malloc\n\
            0000000000001000 T _ZN8wasmtime6Engine3new17h0123456789abcdefE\n\
            0000000000001000 T alias\n\
            0000000000001040 t _ZN4core3fmt5write17h0123456789abcdefE\n\
            0000000000001100 D data\n\
            0000000000001200 T last\n";
        assert_eq!(
            text_symbols(listing),
            vec![
                (
                    0x40,
                    "_ZN8wasmtime6Engine3new17h0123456789abcdefE".to_owned()
                ),
                (0x1c0, "_ZN4core3fmt5write17h0123456789abcdefE".to_owned()),
            ]
        );
    }

    #[test]
    fn symbols_are_attributed_by_their_leading_and_named_paths() {
        assert_eq!(
            path_heads("<wasmtime::Foo as core::fmt::Debug>::fmt"),
            ["wasmtime", "core"]
        );
        let crates: BTreeSet<String> = ["wasmtime".to_owned()].into();
        let symbols = vec![
            (
                100,
                "_ZN8wasmtime6Engine3new17h0123456789abcdefE".to_owned(),
            ),
            (
                10,
                "_ZN4core3ptr13drop_in_place17h0123456789abcdefE".to_owned(),
            ),
            (5, "plain_c_symbol".to_owned()),
        ];
        let (total, leading, mentioning, by_crate) = attribute(&symbols, &crates);
        assert_eq!((total, leading, mentioning), (115, 100, 100,));
        assert_eq!(by_crate["wasmtime"], 100);
    }

    #[test]
    fn the_timings_page_units_are_summed_for_the_packages() {
        let html = r#"<script>
const UNIT_DATA = [{"i":0,"name":"wasmtime","version":"1.0.0","start":1.0,"duration":2.5},
{"i":1,"name":"serde","version":"1.0.0","start":0.0,"duration":4.0},
{"i":2,"name":"cranelift-codegen","version":"1.0.0","start":2.0,"duration":3.0}];
const CONCURRENCY_DATA = [];
</script>"#;
        let units = timing_units(html).unwrap();
        assert_eq!(units.len(), 3);
        let packages = [
            Package {
                name: "wasmtime".into(),
                version: "1.0.0".into(),
                crate_name: None,
            },
            Package {
                name: "cranelift-codegen".into(),
                version: "1.0.0".into(),
                crate_name: None,
            },
        ];
        assert_eq!(unit_figures(&units, &packages), (5.5, 2, 4.0));
        assert!(timing_units("<html></html>").is_err());
    }
}
