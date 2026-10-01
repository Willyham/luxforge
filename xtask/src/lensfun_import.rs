//! Offline, reproducible conversion of the pinned Lensfun version-1 database. This command
//! never downloads profiles; every XML input is checked against its pinned Git blob identity.
use crate::*;
use roxmltree::{Document, Node};
use std::collections::{BTreeSet, HashMap};

const TAG: &str = "v0.3.4";
const COMMIT: &str = "101c745e847a5de4a1e569a94368ce2027198598";
const MAX_XML: usize = 16 * 1024 * 1024;
const MAX_RECORDS: usize = 10_000;
const MAX_CALIBRATIONS: usize = 256;

fn values(node: Node<'_, '_>, name: &str) -> Vec<String> {
    let mut result: Vec<_> = node
        .children()
        .filter(|n| n.has_tag_name(name) && n.attribute("lang").is_none())
        .filter_map(|n| n.text())
        .map(|s| s.trim().to_owned())
        .collect();
    result.sort();
    result.dedup();
    result
}
fn text(node: Node<'_, '_>, name: &str) -> Result<String> {
    values(node, name)
        .into_iter()
        .next()
        .ok_or_else(|| format!("Missing {name}").into())
}
fn number(node: Node<'_, '_>, name: &str) -> Result<f64> {
    let value = text(node, name)?.parse::<f64>()?;
    ensure(value.is_finite() && value > 0.0, format!("Invalid {name}"))?;
    Ok(value)
}
fn canonical(value: &Value) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(value)?)
}
fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn blob(path: &Path) -> Result<String> {
    Ok(output(
        path.parent().ok_or("XML parent")?,
        "git",
        &["hash-object", path.to_str().ok_or("XML path")?],
    )?
    .trim()
    .to_owned())
}
fn sort(values: &mut [Value]) {
    values.sort_by_cached_key(|v| serde_json::to_string(v).expect("JSON value"));
}

fn lens_record(node: Node<'_, '_>) -> Result<std::result::Result<Value, &'static str>> {
    if values(node, "type")
        .first()
        .is_some_and(|v| v != "rectilinear")
    {
        return Ok(Err("unsupported-projection"));
    }
    if node.children().any(|n| n.has_tag_name("center")) {
        return Ok(Err("centre-offset"));
    }
    let distortions: Vec<_> = node
        .descendants()
        .filter(|n| n.has_tag_name("distortion"))
        .collect();
    if distortions.is_empty() {
        return Ok(Err("no-distortion"));
    }
    if distortions.len() > MAX_CALIBRATIONS {
        return Ok(Err("too-many-calibrations"));
    }
    let model = distortions[0]
        .attribute("model")
        .ok_or("Missing distortion model")?;
    if !["poly3", "poly5", "ptlens"].contains(&model)
        || distortions
            .iter()
            .any(|d| d.attribute("model") != Some(model))
    {
        return Ok(Err("mixed-distortion-models"));
    }
    let mut entries = Vec::new();
    for distortion in distortions {
        let focal: f64 = distortion
            .attribute("focal")
            .ok_or("Missing focal")?
            .parse()?;
        ensure(focal.is_finite() && focal > 0.0, "Invalid focal")?;
        let attributes = match model {
            "poly3" => ["k1", "", ""],
            "poly5" => ["k1", "k2", ""],
            _ => ["a", "b", "c"],
        };
        let mut terms = [0.0_f64; 3];
        for (i, attribute) in attributes.into_iter().enumerate() {
            if !attribute.is_empty() {
                terms[i] = distortion.attribute(attribute).unwrap_or("0").parse()?;
            }
            ensure(terms[i].is_finite(), "Invalid distortion coefficient")?;
        }
        // Collapse only byte-identical XML duplicates. Different spelling at the same focal
        // remains ambiguous even if it happens to parse to the same floating-point value.
        let xml = distortion.document().input_text()[distortion.range()].to_owned();
        entries.push((focal, terms, xml));
    }
    entries.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.2.cmp(&b.2)));
    entries.dedup_by(|a, b| a.2 == b.2);
    if entries.windows(2).any(|p| p[0].0 == p[1].0) {
        return Ok(Err("duplicate-focal-calibration"));
    }
    let aspect_ratio = match values(node, "aspect-ratio").first() {
        None => 1.5,
        Some(value) => match value.split_once(':') {
            Some((a, b)) => a.parse::<f64>()? / b.parse::<f64>()?,
            None => value.parse()?,
        },
    };
    ensure(
        aspect_ratio.is_finite() && aspect_ratio >= 1.0,
        "Invalid aspect ratio",
    )?;
    let calibrations: Vec<_> = entries
        .into_iter()
        .map(|(focal_mm, terms, _)| json!({"focal_mm":focal_mm,"terms":terms}))
        .collect();
    let record = json!({"maker":text(node,"maker")?,"models":values(node,"model"),"mounts":values(node,"mount"),"crop_factor":number(node,"cropfactor")?,"aspect_ratio":aspect_ratio,"model":model,"calibrations":calibrations});
    ensure(
        !record["models"].as_array().unwrap().is_empty(),
        "Lens has no model",
    )?;
    let record_sha256 = sha256(&canonical(&record)?);
    let mut keyed = record;
    keyed["key"] = json!(format!("lf1-{}", &record_sha256[..16]));
    keyed["record_sha256"] = json!(record_sha256);
    Ok(Ok(keyed))
}

pub fn import(source: &Path, destination: &Path, expected: &HashMap<&str, &str>) -> Result<String> {
    let directory = source.join("data/db");
    let actual: BTreeSet<_> = fs::read_dir(&directory)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|s| s == "xml"))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    let names: BTreeSet<_> = expected.keys().map(|s| (*s).to_owned()).collect();
    ensure(
        actual == names,
        "Lensfun XML inventory differs from the pinned input table",
    )?;
    let mut inputs = Vec::new();
    let mut total = 0_usize;
    for name in names {
        let path = directory.join(&name);
        let length = fs::metadata(&path)?.len() as usize;
        total = total
            .checked_add(length)
            .ok_or("Lensfun XML input exceeds 16 MiB")?;
        ensure(total <= MAX_XML, "Lensfun XML input exceeds 16 MiB")?;
        let identity = blob(&path)?;
        ensure(
            identity == expected[name.as_str()],
            format!("Lensfun input hash differs: data/db/{name}"),
        )?;
        let bytes = fs::read(&path)?;
        inputs.push((name, identity, bytes));
    }
    let (mut mounts, mut cameras, mut lenses, mut excluded, mut files) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut count = 0;
    for (name, identity, bytes) in inputs {
        let source_text = std::str::from_utf8(&bytes)?;
        let document = Document::parse(source_text)?;
        let root = document.root_element();
        ensure(
            root.has_tag_name("lensdatabase") && root.attribute("version") == Some("1"),
            "Unsupported Lensfun database version",
        )?;
        for node in root.children().filter(Node::is_element) {
            count += 1;
            ensure(count <= MAX_RECORDS, "Lensfun input exceeds 10,000 records")?;
            match node.tag_name().name() {
                "mount" => {
                    let mount_name = text(node,"name")?;
                    let fixed = mount_name.as_bytes().first().is_some_and(u8::is_ascii_lowercase);
                    mounts.push(json!({"name":mount_name,"compat":values(node,"compat"),"fixed":fixed}));
                }
                "camera" => cameras.push(json!({"maker":text(node,"maker")?,"model":text(node,"model")?,"mount":text(node,"mount")?,"crop_factor":number(node,"cropfactor")?})),
                "lens" => match lens_record(node)? {
                    Ok(lens) => lenses.push(lens),
                    Err(reason) => excluded.push(json!({"file":format!("data/db/{name}"),"maker":text(node,"maker")?,"models":values(node,"model"),"reason":reason}))
                },
                _ => return Err("Unknown Lensfun record type".into())
            }
        }
        files.push(json!({"path":format!("data/db/{name}"),"blob_sha1":identity,"sha256":sha256(&bytes),"bytes":bytes.len()}));
    }
    for values in [
        &mut mounts,
        &mut cameras,
        &mut lenses,
        &mut excluded,
        &mut files,
    ] {
        sort(values);
    }
    let index = canonical(&json!({"mounts":mounts,"cameras":cameras,"lenses":lenses}))?;
    let index_sha256 = sha256(&index);
    let provenance = canonical(
        &json!({"tag":TAG,"commit":COMMIT,"files":files,"index_sha256":index_sha256,"excluded":excluded}),
    )?;
    // No output is written until every input, record and whole-import cap has been checked.
    fs::create_dir_all(destination)?;
    fs::write(destination.join("index.json"), index)?;
    fs::write(destination.join("provenance.json"), provenance)?;
    fs::write(
        destination.join("LICENSE-CC-BY-SA-3.0.txt"),
        include_bytes!("lensfun-license.txt"),
    )?;
    fs::write(
        destination.join("ATTRIBUTION.md"),
        format!(
            "# Lensfun database attribution\n\nSource: Lensfun {TAG}, commit `{COMMIT}`.\nhttps://github.com/lensfun/lensfun/tree/{TAG}/data/db\n\nCopyright the Lensfun database contributors. Database data is licensed under Creative Commons Attribution-ShareAlike 3.0 Unported (CC BY-SA 3.0): https://creativecommons.org/licenses/by-sa/3.0/\n\nLuxforge's index is a modified, filtered conversion of the version-1 XML database. It retains only rectilinear distortion profiles with the documented supported models; mounts, camera identities and untranslated lens aliases are retained. See provenance.json for input identities, exclusions and the index hash. The derived index is distributed under CC BY-SA 3.0. Luxforge project code is separately GPL-3.0-or-later. Manual license review remains deferred.\n"
        ),
    )?;
    Ok(index_sha256)
}

pub fn run(root: &Path, source: &Path, destination: &Path) -> Result {
    let hash = import(source, destination, &PINNED_FILES.iter().copied().collect())?;
    fs::write(
        root.join("crates/luxforge-core/src/modules/lens/pinned.rs"),
        format!(
            "//! Generated by cargo xtask lensfun-import; the accepted offline resource identity.\npub(crate) const RELEASE: &str = \"lensfun-0.3.4\";\npub(crate) const COMMIT: &str = \"{COMMIT}\";\npub(crate) const INDEX_SHA256: &str = \"{hash}\";\n"
        ),
    )?;
    println!("Lensfun index: {hash}");
    Ok(())
}

// GitHub's v0.3.4 data/db inventory, pinned to commit 101c745e847a5de4a1e569a94368ce2027198598.
const PINNED_FILES: &[(&str, &str)] = &[
    ("6x6.xml", "b7d75c58d97fecd40ad7e337707f4246ac9a51e0"),
    ("actioncams.xml", "420ef0708e0aab42cb8790fdf6f74a5391a539cb"),
    (
        "compact-canon.xml",
        "fd4f6a4b9920a6de751e1273fbefd082e8d6d672",
    ),
    (
        "compact-casio.xml",
        "1c64d68df22525c525b3e931e7609783a1d26117",
    ),
    (
        "compact-fujifilm.xml",
        "afbcd6447aef12cded27a77c3587b8919e756514",
    ),
    (
        "compact-kodak.xml",
        "666fb388129bcd5385c9637e7b15d225232650dc",
    ),
    (
        "compact-konica-minolta.xml",
        "03d55ec4e488f127be2ab0f875766fc8b26126e5",
    ),
    (
        "compact-leica.xml",
        "f6b84908d8e589cee1349573a7607bf740b827ca",
    ),
    (
        "compact-nikon.xml",
        "fcd57367110f9c3b671fea94d46bee88bf2855ac",
    ),
    (
        "compact-olympus.xml",
        "f9577c0d57e924c5b5917881afce56691e11b2eb",
    ),
    (
        "compact-panasonic.xml",
        "6556fed24e41355d9cba146e227bab7ddf10d9a5",
    ),
    (
        "compact-pentax.xml",
        "3dde5f412432235b5f7eaf4bedafe5cdcfd231d6",
    ),
    (
        "compact-ricoh.xml",
        "ae8b62b86a76d95a9a859d9ba8084dbba1336218",
    ),
    (
        "compact-samsung.xml",
        "8668632b0631d38109257ed248101b138536c087",
    ),
    (
        "compact-sigma.xml",
        "32843f0d0a36b8a672cfeda82be72f1ce2c61ebc",
    ),
    (
        "compact-sony.xml",
        "f40f066241f0b75471177dc29bdf6d4d3ef5dcb6",
    ),
    ("contax.xml", "29189ebd8b7b052aaf4c50357e2735cd6dfcf744"),
    ("generic.xml", "1cd96efa8af83f3ae4ab524339d8c4c0bed6167a"),
    ("mil-canon.xml", "7991c2f80f578b7ef4a6bdc551d167345ad86c7c"),
    (
        "mil-fujifilm.xml",
        "5ef4849072b72dcd5c6fe83cebda0f3e96432634",
    ),
    ("mil-leica.xml", "a5cffcacac8dec3e4339fb4970db6051b2c9c344"),
    ("mil-nikon.xml", "22e1813c396ea8466ddb3ab108dded299e08f722"),
    (
        "mil-olympus.xml",
        "5979b96d962224738fa2ee8d8fe5db258e5cdc23",
    ),
    (
        "mil-panasonic.xml",
        "ec71fed9368ff75d4b5c280cbfb00b63dcde9b3a",
    ),
    ("mil-pentax.xml", "4bfe4f27cc0ab9c96662bb6878a7948a94019173"),
    (
        "mil-samsung.xml",
        "202135ee35bdb3a8a7c7fdea12e92ec59abf0f4d",
    ),
    (
        "mil-samyang.xml",
        "ffb79e3a4d97325ecf5e750efae146b356e9d2c6",
    ),
    ("mil-sigma.xml", "afd2b9eb3e3d2d9d9e1666b5a6ee8dd3a6bb65d8"),
    ("mil-sony.xml", "b674bbaa9750b577d6d55c942b661c714df55258"),
    ("mil-tamron.xml", "6a5a524a04f3233428da7ba5642b4237c70aa638"),
    ("mil-tokina.xml", "a868690525485ffc97b20efc6a77e230fc074e39"),
    ("mil-zeiss.xml", "70eb24c0e93dd09b4b38ae34e445555f8ea4bac3"),
    ("misc.xml", "9aa4353fd198a61615f4060288240c3a7f93ad83"),
    ("om-system.xml", "ae29bdc8b92af3404fb5b28f691af853e3333799"),
    ("rf-leica.xml", "e3cc5a1d7429a249e55235aad168fa9cdbad9a52"),
    ("slr-canon.xml", "3b238fa4dfe22dd48d92546148cf0a1a4f101a2e"),
    (
        "slr-hasselblad.xml",
        "2226afe4857927a8fea3f0f70b34f449e0f45ad2",
    ),
    (
        "slr-konica-minolta.xml",
        "d8076d5fa608d6b0b0605a07f8ccef0b738dc2f2",
    ),
    ("slr-leica.xml", "3d166bea8537cc4966fa46969670e8157070650b"),
    ("slr-nikon.xml", "efeb505aa1ea3287209375f0fb33e4ceb356dbb2"),
    (
        "slr-olympus.xml",
        "e60326cb18e3719cae4d2582407d77eaecfec4c7",
    ),
    (
        "slr-panasonic.xml",
        "7546b1d7183d8c30744143fc786cf3d9d4e0d7ba",
    ),
    ("slr-pentax.xml", "6ee88ed8d3c6d204e0f96ccc2f4d446866a0981a"),
    ("slr-ricoh.xml", "df3b129b433fdb2c0fcf6ee50ad3e65c3464be99"),
    (
        "slr-samsung.xml",
        "5b7934feb7f57a755b84308f930bcfa657d73a85",
    ),
    (
        "slr-samyang.xml",
        "d7e8f560c998cb220c4bf106ffa09dfaf0408b7b",
    ),
    (
        "slr-schneider.xml",
        "fd29f08343475e2c7672da72d6ccf6c5415f45fe",
    ),
    ("slr-sigma.xml", "7d089d972d657eb8cd3e34570316f290cf417611"),
    (
        "slr-soligor.xml",
        "6b298bf11b0975929d01ae2df21fb161ae36fe8a",
    ),
    ("slr-sony.xml", "ff72da5ffec90337c899f51629cd3c2d2c28b53a"),
    ("slr-tamron.xml", "4c13d526647cc526209f56deabff0f3fd12505d0"),
    ("slr-tokina.xml", "5ce39e72264312cfb4913e2fcdf345263611a0fb"),
    ("slr-ussr.xml", "8d98b55609eb876a45addc1b18292d3a20be4c43"),
    (
        "slr-vivitar.xml",
        "004282ffb138957bd33299bfd7c5909e5795d477",
    ),
    ("slr-zeiss.xml", "50c65dc1683dcd6fc6b9cf6a4a67499cc3585818"),
];

#[cfg(test)]
mod tests {
    use super::*;
    fn convert(xml: &str) -> Result<(tempfile::TempDir, Value, Value)> {
        let temp = tempfile::tempdir()?;
        fs::create_dir_all(temp.path().join("data/db"))?;
        let input = temp.path().join("data/db/test.xml");
        fs::write(&input, xml)?;
        let identity = blob(&input)?;
        import(
            temp.path(),
            &temp.path().join("out"),
            &HashMap::from([("test.xml", identity.as_str())]),
        )?;
        let index = read_json(&temp.path().join("out/index.json"))?;
        let provenance = read_json(&temp.path().join("out/provenance.json"))?;
        Ok((temp, index, provenance))
    }
    fn lens(extra: &str, calibrations: &str) -> String {
        format!(
            "<lens><maker>Example</maker><model>Example zoom</model><mount>Example</mount><cropfactor>1</cropfactor>{extra}<calibration>{calibrations}</calibration></lens>"
        )
    }
    fn tree(content: &str) -> String {
        format!("<lensdatabase version=\"1\">{content}</lensdatabase>")
    }
    const CAL: &str = "<distortion model=\"poly3\" focal=\"24\" k1=\"-0.02\"/>";
    #[test]
    fn lensfun_import_is_deterministic() {
        let xml = tree(&lens("", CAL));
        let (temp, _, _) = convert(&xml).unwrap();
        let before = fs::read(temp.path().join("out/index.json")).unwrap();
        let provenance = fs::read(temp.path().join("out/provenance.json")).unwrap();
        let identity = blob(&temp.path().join("data/db/test.xml")).unwrap();
        import(
            temp.path(),
            &temp.path().join("out"),
            &HashMap::from([("test.xml", identity.as_str())]),
        )
        .unwrap();
        assert_eq!(
            before,
            fs::read(temp.path().join("out/index.json")).unwrap()
        );
        assert_eq!(
            provenance,
            fs::read(temp.path().join("out/provenance.json")).unwrap()
        );
    }
    #[test]
    fn lensfun_import_refuses_changed_input_hash() {
        let (temp, _, _) = convert(&tree(&lens("", CAL))).unwrap();
        let input = temp.path().join("data/db/test.xml");
        let identity = blob(&input).unwrap();
        fs::write(input, tree(&lens("<aspect-ratio>4:3</aspect-ratio>", CAL))).unwrap();
        let error = import(
            temp.path(),
            &temp.path().join("new"),
            &HashMap::from([("test.xml", identity.as_str())]),
        )
        .unwrap_err();
        assert!(error.to_string().contains("data/db/test.xml"));
        assert!(!temp.path().join("new").exists());
    }
    #[test]
    fn lensfun_import_excludes_oversized_record_and_keeps_others() {
        let calibrations: String = (1..=257)
            .map(|f| format!("<distortion model=\"poly3\" focal=\"{f}\" k1=\"0\"/>"))
            .collect();
        let (_, index, provenance) =
            convert(&tree(&(lens("", &calibrations) + &lens("", CAL)))).unwrap();
        assert_eq!(index["lenses"].as_array().unwrap().len(), 1);
        assert_eq!(provenance["excluded"][0]["reason"], "too-many-calibrations");
    }
    #[test]
    fn lensfun_import_excludes_mixed_and_duplicate_focal_records() {
        let mixed = format!("{CAL}<distortion model=\"poly5\" focal=\"70\" k1=\"0\"/>");
        let duplicate = format!("{CAL}<distortion model=\"poly3\" focal=\"24\" k1=\"-0.01\"/>");
        let identical = format!("{CAL}{CAL}");
        let (_, index, p) = convert(&tree(
            &(lens("", &mixed) + &lens("", &duplicate) + &lens("", &identical)),
        ))
        .unwrap();
        assert_eq!(index["lenses"].as_array().unwrap().len(), 1);
        assert_eq!(
            index["lenses"][0]["calibrations"].as_array().unwrap().len(),
            1
        );
        let reasons: Vec<_> = p["excluded"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["reason"].as_str().unwrap())
            .collect();
        assert!(reasons.contains(&"mixed-distortion-models"));
        assert!(reasons.contains(&"duplicate-focal-calibration"));
    }
    #[test]
    fn lensfun_import_refuses_centre_offset_records() {
        let (_, index, p) = convert(&tree(&lens("<center x=\"0.01\" y=\"0\"/>", CAL))).unwrap();
        assert!(index["lenses"].as_array().unwrap().is_empty());
        assert_eq!(p["excluded"][0]["reason"], "centre-offset");
    }
    #[test]
    fn lensfun_import_fails_only_on_whole_import_caps() {
        assert!(convert(&tree(&lens("<type>fisheye</type>", CAL))).is_ok());
        let too_many = "<mount><name>Example</name></mount>".repeat(MAX_RECORDS + 1);
        assert!(
            convert(&tree(&too_many))
                .unwrap_err()
                .to_string()
                .contains("10,000 records")
        );
        assert!(
            convert(&tree(&format!("<!--{}-->", " ".repeat(MAX_XML))))
                .unwrap_err()
                .to_string()
                .contains("16 MiB")
        );
    }
    #[test]
    fn lensfun_import_marks_lowercase_mounts_fixed() {
        let (_,index,_)=convert(&tree("<mount><name>djiFC3411</name></mount><mount><name>Nikon Z</name><compat>Nikon F</compat></mount>")).unwrap();
        for mount in index["mounts"].as_array().unwrap() {
            assert_eq!(mount["fixed"], mount["name"] == "djiFC3411");
        }
    }
    #[test]
    fn lensfun_import_keys_duplicate_model_records_distinctly() {
        let (_, index, _) = convert(&tree(
            &(lens("", CAL) + &lens("<aspect-ratio>4:3</aspect-ratio>", CAL)),
        ))
        .unwrap();
        assert_ne!(index["lenses"][0]["key"], index["lenses"][1]["key"]);
    }
}
