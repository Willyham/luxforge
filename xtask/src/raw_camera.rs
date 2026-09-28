//! `raw-camera-metadata`: fetch a selected subset of raw.pixls.us's samples and read each with the
//! RAW adapter, the evidence behind adding a camera to the catalog. It never mirrors the corpus:
//! every requested ID must be in the repository index with a declared CC0 licence and a SHA-256.
//! Each download is bounded, streamed into a new file and checked against that hash; each passing
//! file is then decoded once by `luxforge_raw::RawSource`, which reports its metadata or why the
//! adapter refuses it (an unrecognised mode names LibRaw's make, model, decoder, bit depth and
//! size), and is hashed again to show it unchanged. Originals and `results.json` stay in a new
//! output directory, which belongs outside the repository.
use crate::*;
use luxforge_raw::{MAX_SOURCE_BYTES, RawSource};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use regex::Regex;
use serde_json::Map;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::Write,
    process::Stdio,
    sync::atomic::AtomicBool,
};

const MAX_INDEX: u64 = 64 * 1024 * 1024;
const MAX_SAMPLES: usize = 128;
const MIB: u64 = 1024 * 1024;
/// The largest per-source download cap, the RAW adapter's own encoded-source bound.
pub const MAX_SOURCE_MIB: u64 = MAX_SOURCE_BYTES as u64 / MIB;
/// The default per-source download cap.
pub const DEFAULT_SOURCE_MIB: u64 = 128;
const USER_AGENT: &str = "Luxforge-camera-qualification/1";
const CC0: &str = "creativecommons.org/publicdomain/zero/1.0/";

/// One selected index entry.
#[derive(Clone, Debug, PartialEq)]
struct Sample {
    id: String,
    make: String,
    model: String,
    mode: String,
    url: String,
    sha256: String,
}

impl Sample {
    fn record(&self) -> Map<String, Value> {
        let Value::Object(record) = json!({
            "id": self.id, "make": self.make, "model": self.model, "mode": self.mode,
            "source_url": self.url, "sha256": self.sha256, "license": "CC0-1.0",
        }) else {
            unreachable!()
        };
        record
    }
}

/// The few character references the index's attribute values use, decoded; anything else is
/// kept as written.
fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let decoded = rest.find(';').and_then(|end| {
            let name = &rest[1..end];
            let c = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ => name
                    .strip_prefix("#x")
                    .or_else(|| name.strip_prefix("#X"))
                    .map(|hex| u32::from_str_radix(hex, 16))
                    .or_else(|| name.strip_prefix('#').map(str::parse::<u32>))
                    .and_then(|code| code.ok())
                    .and_then(char::from_u32),
            }?;
            Some((c, end))
        });
        match decoded {
            Some((c, end)) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The requested entries of a raw.pixls.us repository index (`json/getrepository.php`), in
/// numeric ID order. Every one must be present, declared CC0 and carry a SHA-256.
fn entries(index: &Value, ids: &BTreeSet<String>) -> Result<Vec<Sample>> {
    let href = Regex::new(r"href='([^']+)'")?;
    let number = Regex::new(r"/getfile.php/(\d+)/")?;
    let digest = Regex::new(r"sha256 Checksum'>([0-9a-f]{64})")?;
    let rows = index["data"]
        .as_array()
        .ok_or("the index has no data rows")?;
    let mut found = BTreeMap::new();
    for row in rows {
        let text = |i: usize| row[i].as_str().unwrap_or_default();
        let Some(link) = href.captures(text(7)) else {
            continue;
        };
        let url = unescape(&link[1]);
        let Some(id) = number.captures(&url).map(|n| n[1].to_owned()) else {
            continue;
        };
        if !ids.contains(&id) {
            continue;
        }
        ensure(
            text(5).contains(CC0),
            format!("sample {id} is not declared CC0"),
        )?;
        let sha256 = digest
            .captures(text(7))
            .ok_or(format!("sample {id} has no SHA-256"))?[1]
            .to_owned();
        let sample = Sample {
            id: id.clone(),
            make: text(0).into(),
            model: text(1).into(),
            mode: text(2).into(),
            url,
            sha256,
        };
        found.insert(id, sample);
    }
    let missing: Vec<&String> = ids.iter().filter(|id| !found.contains_key(*id)).collect();
    ensure(
        missing.is_empty(),
        format!("missing sample IDs: {missing:?}"),
    )?;
    let mut samples: Vec<Sample> = found.into_values().collect();
    samples.sort_by_key(|sample| sample.id.parse::<u64>().unwrap_or(u64::MAX));
    Ok(samples)
}

/// `url` with its path percent-encoded, `/` kept: the index writes spaces in its file names.
fn quoted(url: &str) -> Result<(String, String)> {
    /// Everything but the unreserved characters and `/`.
    const PATH: &AsciiSet = &NON_ALPHANUMERIC
        .remove(b'_')
        .remove(b'.')
        .remove(b'-')
        .remove(b'~')
        .remove(b'/');
    let scheme_end = url.find("://").ok_or(format!("{url} has no scheme"))? + 3;
    let rest = &url[scheme_end..];
    let path_start = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let after = &rest[path_start..];
    let path_end = after.find(['?', '#']).unwrap_or(after.len());
    let path = &after[..path_end];
    Ok((
        format!(
            "{}{}{}",
            &url[..scheme_end + path_start],
            utf8_percent_encode(path, PATH),
            &after[path_end..]
        ),
        path.to_owned(),
    ))
}

/// Stream `url` through `curl` (HTTPS only, redirects included) into a new file at `path`,
/// refusing more than `limit` bytes, and check the bytes against `expected`.
fn download(url: &str, path: &Path, limit: u64, expected: &str) -> Result<u64> {
    let mut child = Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--fail",
            "--location",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--connect-timeout",
            "60",
            "--speed-time",
            "60",
            "--speed-limit",
            "1",
            "--user-agent",
            USER_AGENT,
            "--output",
            "-",
            "--url",
            url,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("curl: {e}"))?;
    let mut stdout = child.stdout.take().ok_or("curl has no output")?;
    let (mut target, mut hash, mut total) = (None::<File>, Sha256::new(), 0u64);
    let mut buffer = vec![0; MIB as usize];
    let streamed = loop {
        let n = match stdout.read(&mut buffer) {
            Ok(0) => break Ok(()),
            Ok(n) => n,
            Err(e) => break Err(e.to_string()),
        };
        total += n as u64;
        if total > limit {
            break Err(format!("source exceeds {limit} bytes"));
        }
        if target.is_none() {
            // create_new: a source already on disk is never overwritten.
            match File::create_new(path) {
                Ok(file) => target = Some(file),
                Err(e) => break Err(e.to_string()),
            }
        }
        if let Err(e) = target.as_mut().unwrap().write_all(&buffer[..n]) {
            break Err(e.to_string());
        }
        hash.update(&buffer[..n]);
    };
    if streamed.is_err() {
        let _ = child.kill();
    }
    drop(stdout);
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    let status = child.wait()?;
    streamed?;
    ensure(
        status.success(),
        match stderr.trim() {
            "" => format!("curl exited with {status}"),
            message => message.to_owned(),
        },
    )?;
    if target.is_none() {
        File::create_new(path)?;
    }
    let actual = format!("{:x}", hash.finalize());
    ensure(actual == expected, format!("SHA-256 mismatch: {actual}"))?;
    Ok(total)
}

/// Download one sample into `root` as `ID.EXT`, recording the outcome.
fn fetch(sample: &Sample, root: &Path, limit: u64) -> Map<String, Value> {
    let mut result = sample.record();
    let outcome = quoted(&sample.url).and_then(|(url, path)| {
        let suffix = Path::new(&path)
            .extension()
            .map(|ext| format!(".{}", ext.to_string_lossy()))
            .unwrap_or_default();
        let file = root.join(format!("{}{suffix}", sample.id));
        result.insert("path".into(), json!(file));
        download(&url, &file, limit, &sample.sha256)
    });
    match outcome {
        Ok(bytes) => {
            result.insert("bytes".into(), json!(bytes));
            result.insert("download".into(), json!("passed"));
        }
        Err(error) => {
            result.insert("download".into(), json!("failed"));
            result.insert("error".into(), json!(error.to_string()));
        }
    }
    println!(
        "{} {} {} {}",
        sample.id,
        sample.make,
        sample.model,
        result["download"].as_str().unwrap()
    );
    result
}

/// Decode one downloaded source with the RAW adapter and hash it again.
fn decode(result: &mut Map<String, Value>) {
    let path = PathBuf::from(result["path"].as_str().unwrap());
    let decoded = fs::metadata(&path)
        .map_err(|e| e.to_string())
        .and_then(|m| {
            if m.len() > MAX_SOURCE_BYTES as u64 {
                return Err(format!("source exceeds {MAX_SOURCE_MIB} MiB"));
            }
            let bytes = fs::read(&path).map_err(|e| e.to_string())?;
            let raw =
                RawSource::decode(bytes, &AtomicBool::new(false)).map_err(|e| e.to_string())?;
            serde_json::to_value(raw.metadata()).map_err(|e| e.to_string())
        });
    match decoded {
        Ok(metadata) => {
            result.insert("decode".into(), json!("passed"));
            result.insert("metadata".into(), metadata);
        }
        Err(error) => {
            result.insert("decode".into(), json!("failed"));
            result.insert("decode_error".into(), json!(error));
        }
    }
    let preserved = hash(&path).is_ok_and(|actual| result["sha256"] == actual.as_str());
    result.insert("source_preserved".into(), json!(preserved));
    println!(
        "{} decode {}",
        result["id"].as_str().unwrap(),
        result["decode"].as_str().unwrap()
    );
}

/// `cargo xtask raw-camera-metadata --index FILE --ids ID[,ID...] --output NEW_DIR
/// [--max-source-mib N]`: 1 to 128 samples, each at most `N` MiB (default 128, at most 512).
/// Fails unless every sample downloads, decodes and stays unchanged; `results.json` records each.
pub fn run(index: &Path, ids: &[String], out: &Path, max_source_mib: u64) -> Result {
    let ids: BTreeSet<String> = ids.iter().cloned().collect();
    ensure(
        (1..=MAX_SAMPLES).contains(&ids.len()) && (1..=MAX_SOURCE_MIB).contains(&max_source_mib),
        format!("select 1..{MAX_SAMPLES} samples, bounded to at most {MAX_SOURCE_MIB} MiB each"),
    )?;
    let mut text = Vec::new();
    File::open(index)?
        .take(MAX_INDEX + 1)
        .read_to_end(&mut text)?;
    ensure(text.len() as u64 <= MAX_INDEX, "the index exceeds 64 MiB")?;
    let samples = entries(&serde_json::from_slice(&text)?, &ids)?;
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent)?;
    }
    // create_dir, not create_dir_all: an existing evidence directory is never reused.
    fs::create_dir(out)?;
    let root = fs::canonicalize(out)?;
    let mut results: Vec<Map<String, Value>> = samples
        .iter()
        .map(|sample| fetch(sample, &root, max_source_mib * MIB))
        .collect();
    let report = root.join("results.json");
    for i in 0..results.len() {
        if results[i]["download"] != "passed" {
            continue;
        }
        decode(&mut results[i]);
        write_json(&report, &json!(results))?;
    }
    write_json(&report, &json!(results))?;
    ensure(
        results
            .iter()
            .all(|r| r.get("decode") == Some(&json!("passed")) && r["source_preserved"] == true),
        format!("RAW camera metadata incomplete; see {}", report.display()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, license: &str, digest: &str) -> Value {
        json!([
            "Nikon",
            "Z 6",
            "14bit lossless",
            28.77,
            "",
            license,
            "2019-01-01",
            format!(
                "<a href='https://raw.pixls.us/getfile.php/{id}/nice/Nikon - Z 6 - a&amp;b.NEF'>x</a>\
                <div class='checksumdata'><span title='sha256 Checksum'>{digest}</span></div>"
            )
        ])
    }

    fn ids(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    #[test]
    fn selected_entries_are_cc0_hashed_and_in_numeric_order() {
        let cc0 = "<a href='https://creativecommons.org/publicdomain/zero/1.0/'>co</a>";
        let index = json!({"data": [
            row("3582", cc0, &"a".repeat(64)),
            row("900", cc0, &"b".repeat(64)),
            row("77", "<a href='https://example.org/other'>by</a>", &"c".repeat(64)),
            ["no link", "", "", 0, "", "", "", ""],
        ]});
        let samples = entries(&index, &ids(&["3582", "900"])).unwrap();
        assert_eq!(
            samples.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["900", "3582"]
        );
        assert_eq!(
            samples[1].url,
            "https://raw.pixls.us/getfile.php/3582/nice/Nikon - Z 6 - a&b.NEF"
        );
        assert_eq!(samples[1].sha256, "a".repeat(64));
        assert_eq!(samples[1].record()["license"], "CC0-1.0");
        assert!(
            entries(&index, &ids(&["77"]))
                .unwrap_err()
                .to_string()
                .contains("not declared CC0")
        );
        assert!(
            entries(&index, &ids(&["900", "5"]))
                .unwrap_err()
                .to_string()
                .contains("missing sample IDs: [\"5\"]")
        );
        let unhashed = json!({"data": [row("900", cc0, "not-a-digest")]});
        assert!(
            entries(&unhashed, &ids(&["900"]))
                .unwrap_err()
                .to_string()
                .contains("has no SHA-256")
        );
    }

    #[test]
    fn the_path_alone_is_quoted_and_character_references_are_decoded() {
        let (url, path) =
            quoted("https://raw.pixls.us/getfile.php/3582/nice/Nikon - Z 6 (3:2)~_.NEF?x=a b")
                .unwrap();
        assert_eq!(
            url,
            "https://raw.pixls.us/getfile.php/3582/nice/Nikon%20-%20Z%206%20%283%3A2%29~_.NEF?x=a b"
        );
        assert_eq!(path, "/getfile.php/3582/nice/Nikon - Z 6 (3:2)~_.NEF");
        assert_eq!(
            unescape("a&amp;b&#39;c&#x41;&unknown; &"),
            "a&b'cA&unknown; &"
        );
    }

    #[test]
    fn selections_and_output_directories_are_bounded() {
        let tmp = tempfile::tempdir().unwrap();
        let index = tmp.path().join("index.json");
        fs::write(&index, r#"{"data": []}"#).unwrap();
        let out = tmp.path().join("out");
        let many: Vec<String> = (0..129).map(|i| i.to_string()).collect();
        for (ids, mib) in [
            (vec![], 128),
            (many, 128),
            (vec!["1".into()], 0),
            (vec!["1".into()], 513),
        ] {
            assert!(
                run(&index, &ids, &out, mib)
                    .unwrap_err()
                    .to_string()
                    .contains("select 1..128")
            );
        }
        // An unknown ID fails before any output directory is made.
        assert!(run(&index, &["1".into()], &out, 128).is_err());
        assert!(!out.exists());
    }

    #[test]
    fn a_source_the_adapter_refuses_is_recorded_as_failed_and_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("1.NEF");
        fs::write(&path, b"not a raw file").unwrap();
        let mut result = Map::from_iter([
            ("id".to_owned(), json!("1")),
            ("path".to_owned(), json!(path)),
            ("sha256".to_owned(), json!(hash(&path).unwrap())),
        ]);
        decode(&mut result);
        assert_eq!(result["decode"], "failed");
        assert!(result["decode_error"].as_str().unwrap().contains("RAW"));
        assert_eq!(result["source_preserved"], true);
    }
}
