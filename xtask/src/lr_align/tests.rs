use super::*;
use luxforge_test_macros::slow;
use readback::{How, counterpart, read_back};
use std::collections::{BTreeMap, BTreeSet};

fn baseline_of(subject: &Subject) -> Fields {
    settings::baseline(match subject {
        Subject::Target(_) => Kind::Jpeg,
        Subject::Photographs => Kind::Raw,
    })
}

fn every_round() -> Vec<(u32, Vec<Family>)> {
    [1, 2, 3]
        .into_iter()
        .map(|round| (round, rounds::round(round).unwrap()))
        .collect()
}

/// Each kind's baseline reads back with nothing unsupported or refused, every value transfer at
/// its neutral number, and a RAW photograph's white balance As Shot.
#[test]
fn the_baselines_read_back_neutral() {
    let registry = ModuleRegistry::builtin();
    for kind in [Kind::Jpeg, Kind::Raw] {
        let fields = settings::baseline(kind);
        let read = read_back(
            &fields,
            &settings::xmp(&fields, kind == Kind::Jpeg),
            &registry,
        )
        .unwrap_or_else(|error| panic!("{kind:?}: {error}"));
        assert!(
            read.unsupported.is_empty(),
            "{kind:?}: {:?}",
            read.unsupported
        );
        assert_eq!(read.settings["set-basic"]["exposure"], json!(0.0));
        assert_eq!(read.settings["set-mixer"]["grade-blending"], json!(50));
        assert_eq!(
            read.settings["set-curve"]["luminance"],
            json!([[0.0, 0.0], [1.0, 1.0]])
        );
        assert_eq!(
            read.settings
                .get("set-raw")
                .map(|raw| &raw["white-balance"]),
            (kind == Kind::Raw).then(|| json!("as-shot")).as_ref()
        );
    }
}

/// The last step of each area's first family in every round, the far end of its range, reads back
/// through the preset reader on the kind of subject it is first applied to; the slow test below
/// reads back every variant.
#[test]
fn every_area_reads_back() {
    let registry = ModuleRegistry::builtin();
    for (round, families) in every_round() {
        let mut areas = BTreeSet::new();
        for family in families.iter().filter(|family| areas.insert(family.area)) {
            let subject = &family.subjects[0];
            let step = family.steps.last().unwrap();
            let fields = compose(&baseline_of(subject), family, step);
            let packet = matches!(subject, Subject::Target(_));
            read_back(&fields, &settings::xmp(&fields, packet), &registry).unwrap_or_else(
                |error| panic!("round {round} {} {}: {error}", family.id, step.label),
            );
        }
    }
}

/// Every step of every family of every round reads back, on every kind of subject it is applied
/// to, and lands on the controls the manifest names.
#[test]
#[slow]
fn every_variant_of_every_round_reads_back() {
    let registry = ModuleRegistry::builtin();
    let mut count = 0;
    for (round, families) in every_round() {
        for family in &families {
            let kinds: BTreeSet<bool> = family
                .subjects
                .iter()
                .map(|subject| matches!(subject, Subject::Target(_)))
                .collect();
            for jpeg in kinds {
                let subject = if jpeg {
                    Subject::Target("wedge")
                } else {
                    Subject::Photographs
                };
                let baseline = baseline_of(&subject);
                for step in &family.steps {
                    let fields = compose(&baseline, family, step);
                    let read = read_back(&fields, &settings::xmp(&fields, jpeg), &registry)
                        .unwrap_or_else(|error| {
                            panic!("round {round} {} {}: {error}", family.id, step.label)
                        });
                    for (name, field) in &step.fields {
                        // A value at its calibration's shortfall is refused, not transferred.
                        let refused = match field {
                            Field::Number { value, .. } => {
                                matches!(
                                    luxforge_core::lightroom_calibration(name, *value),
                                    Some(Err(_))
                                )
                            }
                            _ => false,
                        };
                        if let Some((action, target, How::Transfer)) = counterpart(name)
                            && !refused
                        {
                            assert!(
                                read.settings[action].get(target).is_some(),
                                "{name} on {action}.{target}"
                            );
                        }
                        if let Field::Correction(correction) = field {
                            assert_eq!(
                                settings::corrections(&settings::xmp(&fields, jpeg)).unwrap(),
                                vec![*correction]
                            );
                        }
                    }
                    count += 1;
                }
            }
        }
    }
    assert_eq!(count, 1112);
}

/// The rounds' sizes, which the owner's import and export are planned around: round 1 writes 758
/// variants of the targets and 49 of each photograph, round 2 292 and 21, round 3 553 and 29.
#[test]
fn the_rounds_have_their_sizes() {
    for (round, families) in every_round() {
        let mut targets = BTreeSet::new();
        let mut on_targets = 0;
        let mut per_photograph = 1;
        for family in &families {
            for subject in &family.subjects {
                match subject {
                    Subject::Target(id) => {
                        targets.insert(*id);
                        on_targets += family.steps.len();
                    }
                    Subject::Photographs => per_photograph += family.steps.len(),
                }
            }
        }
        let sizes = (on_targets + targets.len(), per_photograph);
        assert_eq!(
            sizes,
            match round {
                1 => (758, 49),
                2 => (292, 21),
                _ => (553, 29),
            },
            "round {round}"
        );
    }
}

/// Family ids are unique within a round, every step's label within its family, and so every
/// variant id within a round; every label is safe in a file name.
#[test]
fn families_and_variants_have_unique_names() {
    for (round, families) in every_round() {
        let mut ids = BTreeSet::new();
        let mut variants = BTreeSet::new();
        for family in &families {
            assert!(
                ids.insert(family.id.clone()),
                "round {round}: {}",
                family.id
            );
            assert!(family.id != NEUTRAL);
            for step in &family.steps {
                assert!(
                    step.label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"+-._".contains(&b)),
                    "{}",
                    step.label
                );
                for subject in &family.subjects {
                    let name = match subject {
                        Subject::Target(id) => (*id).to_owned(),
                        Subject::Photographs => "pixls-1".to_owned(),
                    };
                    assert!(
                        variants.insert(variant_id(&name, &family.id, &step.label)),
                        "round {round}: {name} {} {}",
                        family.id,
                        step.label
                    );
                }
            }
        }
        // Round 1 covers every area of the design's order; round 2 the seven pairs; round 3 the
        // realigned areas.
        let areas: BTreeSet<&str> = families.iter().map(|family| family.area).collect();
        if round == 3 {
            assert_eq!(
                areas,
                BTreeSet::from(["basic", "mixer", "presence", "vignette"])
            );
        } else if round == 1 {
            assert_eq!(
                areas,
                BTreeSet::from([
                    "basic",
                    "detail",
                    "geometry",
                    "grain",
                    "masks",
                    "mixer",
                    "presence",
                    "tone-curve",
                    "vignette",
                    "white-balance"
                ])
            );
        } else {
            assert_eq!(
                families
                    .iter()
                    .filter(|f| f.subjects != [Subject::Photographs])
                    .count(),
                7
            );
        }
    }
}

/// Every Lightroom setting a round varies is compared with a Luxforge control the builtin
/// registry declares, except the gradient corrections (mask components, not actions) and the lens
/// profile's own qualifiers.
#[test]
fn every_counterpart_is_a_luxforge_control() {
    let registry = ModuleRegistry::builtin();
    let held = [
        "GradientBasedCorrections",
        "CircularGradientBasedCorrections",
        "LensProfileSetup",
        "LensProfileDistortionScale",
        "LensProfileVignettingScale",
        "HasCrop",
        "ToneCurveName2012",
    ];
    let mut names = BTreeSet::new();
    for (_, families) in every_round() {
        for family in &families {
            for (name, _) in family
                .base
                .iter()
                .chain(family.steps.iter().flat_map(|s| &s.fields))
            {
                names.insert(*name);
            }
        }
    }
    for name in names {
        match counterpart(name) {
            Some((action, field, _)) => {
                let (_, descriptor) = registry
                    .action(action)
                    .unwrap_or_else(|| panic!("{name}: no action {action}"));
                assert!(
                    descriptor.parameter(field).is_some(),
                    "{name}: {action} has no {field}"
                );
            }
            None => assert!(held.contains(&name), "{name} has no counterpart"),
        }
    }
}

/// The value texts Lightroom writes, and the labels variants are named by.
#[test]
fn numbers_and_labels_are_written_as_lightroom_writes_them() {
    assert_eq!(settings::number_text(25.0, true, 0), "+25");
    assert_eq!(settings::number_text(-25.0, true, 0), "-25");
    assert_eq!(settings::number_text(0.0, true, 2), "0.00");
    assert_eq!(settings::number_text(-0.0, true, 0), "0");
    assert_eq!(settings::number_text(1.5, true, 2), "+1.50");
    assert_eq!(settings::number_text(1.0, true, 1), "+1.0");
    assert_eq!(settings::number_text(5500.0, false, 0), "5500");
    assert_eq!(settings::number_text(0.25, false, 6), "0.250000");
    assert_eq!(rounds::label(-0.5), "-0.5");
    assert_eq!(rounds::label(0.8), "+0.8");
    assert_eq!(rounds::label(100.0), "+100");
    assert_eq!(rounds::label(0.0), "0");
    // A straightened crop is the largest centred 3:2 rectangle that stays inside the frame at
    // either sign of the angle, and holds exactly the six decimals the XMP writes.
    for angle in [-10.0, -5.0, -1.0, 1.0, 2.0, 10.0] {
        let crop = rounds::straighten_box(angle);
        let (width, height) = rounds::straighten_size(angle, crop);
        assert!(
            (width / height - 1.5).abs() < 1e-4,
            "{angle}: {width} x {height}"
        );
        let mirrored = rounds::straighten_size(-angle, rounds::straighten_box(-angle));
        assert!((mirrored.0 - width).abs() < 0.01 && (mirrored.1 - height).abs() < 0.01);
        for value in crop {
            assert!(value > 0.0 && value < 1.0);
            assert_eq!(
                settings::number_text(value, false, 6)
                    .parse::<f64>()
                    .unwrap(),
                value
            );
        }
    }
    // Lightroom's reading reproduces round 1's exports: its recorded crop for −10° and +10° gave
    // 897 × 857 and 1326 × 582 pixels.
    let (w, h) = rounds::straighten_size(-10.0, [0.155819, 0.155819, 0.844181, 0.844181]);
    assert_eq!((w.round(), h.round()), (897.0, 857.0));
    let (w, h) = rounds::straighten_size(10.0, [0.098484, 0.098484, 0.901516, 0.901516]);
    assert_eq!((w.round(), h.round()), (1326.0, 582.0));
}

/// Each target's pixels, frozen by their SHA-256: the generator is exact, and a change to a target
/// is a reviewed change to this table.
const PIXELS: [(&str, &str); 7] = [
    (
        "wedge",
        "754922630da409b1629600ca68de7bedbd0bbe4d6af17da0c67d1b7d04c803e8",
    ),
    (
        "patches",
        "742477c166fc6f2e7233428455f0c0bf5e634de2ed9aa5d20ff4dbd2f685f309",
    ),
    (
        "surround",
        "e35514124b2d03d49c2f579bca943dea8760fecaa8dd8c57f4703ad2def97ac3",
    ),
    (
        "edges",
        "1daf010d5e73e8621db5a157f2177fdafd01c02586107e32afbf004f0bb6b77d",
    ),
    (
        "flat",
        "02e0bb952db885a842c37fa008d02149bf6fdba5b8325a0227640da45252cfdd",
    ),
    (
        "greys",
        "50b0be0097bc1c0a72f1a40c0d6e240da46a84412f14b1bae071e687cfce5528",
    ),
    (
        "grid",
        "6a50bea5087cadeabd48e834cdd4784b58fe45ac5c120bced7544980ead6deec",
    ),
];

#[test]
#[slow]
fn targets_are_exact() {
    let actual: Vec<(&str, String)> = PIXELS
        .iter()
        .map(|(id, _)| {
            let target = targets::target(id).unwrap();
            assert_eq!(
                target.rgb.len(),
                (target.width * target.height * 3) as usize
            );
            (*id, format!("{:x}", Sha256::digest(&target.rgb)))
        })
        .collect();
    let expected: Vec<(&str, String)> = PIXELS
        .iter()
        .map(|(id, digest)| (*id, (*digest).to_owned()))
        .collect();
    assert_eq!(actual, expected);
}

/// The 24 MP edge chart is the 2400 × 1600 one at two and a half times the side, frozen too.
#[test]
#[slow]
fn the_large_edge_chart_is_exact() {
    let target = targets::target("edges-24mp").unwrap();
    assert_eq!((target.width, target.height), (6000, 4000));
    assert_eq!(
        format!("{:x}", Sha256::digest(&target.rgb)),
        "87f4b86143c60441dbe36fa668c421321844a4775dc0b649bbf1d1a5ff6719e0"
    );
}

/// The targets hold what their layouts say: every wedge code once, a surround patch on its
/// surround, an Oklab patch at its recorded code, a grid dot dark at its centre.
#[test]
#[slow]
fn targets_hold_their_layouts() {
    let pixel = |target: &targets::Target, x: u32, y: u32| {
        let at = ((y * target.width + x) * 3) as usize;
        [target.rgb[at], target.rgb[at + 1], target.rgb[at + 2]]
    };
    for id in ["wedge", "patches", "surround", "greys"] {
        let target = targets::target(id).unwrap();
        let patch = target.layout["patch"].as_u64().unwrap() as u32;
        let patches = target.layout["patches"].as_array().unwrap();
        if id == "wedge" {
            let codes: BTreeSet<u64> = patches
                .iter()
                .map(|p| p["code"][0].as_u64().unwrap())
                .collect();
            assert_eq!(codes.len(), 256);
        }
        if id == "patches" {
            assert_eq!(patches.len(), 480);
        }
        for entry in patches {
            let (x, y) = (
                entry["x"].as_u64().unwrap() as u32,
                entry["y"].as_u64().unwrap() as u32,
            );
            let code: [u8; 3] = std::array::from_fn(|c| entry["code"][c].as_u64().unwrap() as u8);
            assert_eq!(pixel(&target, x + patch / 2, y + patch / 2), code, "{id}");
            assert_eq!(pixel(&target, x, y), code, "{id}");
            assert_eq!(pixel(&target, x + patch - 1, y + patch - 1), code, "{id}");
        }
    }
    let grid = targets::target("grid").unwrap();
    assert_eq!((grid.width, grid.height), (1500, 1000));
    assert_eq!(pixel(&grid, 25, 25), [30; 3]);
    assert_eq!(pixel(&grid, 50, 50), [200; 3]);
    let flat = targets::target("flat").unwrap();
    assert!(flat.rgb.iter().all(|value| *value == 128));
}

/// A variant's XMP segment follows SOI and the JFIF segment, reads back as written, and the file
/// still decodes to the target.
#[test]
fn a_variant_jpeg_carries_its_xmp_and_decodes() {
    let target = targets::target("wedge").unwrap();
    let jpeg = encode(&target).unwrap();
    assert_eq!(&jpeg[..4], &[0xff, 0xd8, 0xff, 0xe0]);
    assert_eq!(embedded_xmp(&jpeg), None);
    let fields = settings::baseline(Kind::Jpeg);
    let xmp = settings::xmp(&fields, true);
    let bytes = splice(&jpeg, &xmp).unwrap();
    let app0 = usize::from(u16::from_be_bytes([jpeg[4], jpeg[5]]));
    assert_eq!(&bytes[4 + app0..6 + app0], &[0xff, 0xe1]);
    assert_eq!(embedded_xmp(&bytes).as_deref(), Some(xmp.as_str()));
    let decoded = image::load_from_memory(&bytes).unwrap().to_rgb8();
    assert_eq!(
        (decoded.width(), decoded.height()),
        (target.width, target.height)
    );
    // Quality 100 without chroma subsampling: every grey patch's centre within a code.
    for entry in target.layout["patches"].as_array().unwrap() {
        let (x, y) = (
            entry["x"].as_u64().unwrap() as u32 + 24,
            entry["y"].as_u64().unwrap() as u32 + 24,
        );
        let code = entry["code"][0].as_u64().unwrap() as u8;
        assert!(
            decoded
                .get_pixel(x, y)
                .0
                .iter()
                .all(|v| v.abs_diff(code) <= 1)
        );
    }
}

/// The corpus selection names qualified, non-DNG samples of five makers; resolving it from a cache
/// that lacks one names the group to sync, and from a cache that holds them all gives each
/// photograph's camera, extension and developed size.
#[test]
fn the_corpus_selection_resolves_from_a_cache() {
    let corpus = read_json(&root().unwrap().join(photos::CORPUS_MANIFEST)).unwrap();
    let mut groups = BTreeSet::new();
    for (id, _) in photos::CORPUS_SELECTION {
        let sample = corpus["samples"]
            .as_array()
            .unwrap()
            .iter()
            .find(|sample| sample["id"] == id)
            .unwrap();
        assert_eq!(sample["expectation"]["kind"], "qualified", "{id}");
        assert!(
            !sample["extension"]
                .as_str()
                .unwrap()
                .eq_ignore_ascii_case(".dng")
        );
        groups.insert(sample["group"].as_str().unwrap().to_owned());
    }
    assert_eq!(groups.len(), 5);
    let cache = tempfile::tempdir().unwrap();
    let error = photos::corpus(&corpus, cache.path())
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("pixls-3582 (Nikon Z 6, group nikon)"),
        "{error}"
    );
    for (id, _) in photos::CORPUS_SELECTION {
        let sample = corpus["samples"]
            .as_array()
            .unwrap()
            .iter()
            .find(|sample| sample["id"] == id)
            .unwrap();
        let name = format!(
            "{}{}",
            sample["sha256"].as_str().unwrap(),
            sample["extension"].as_str().unwrap().to_lowercase()
        );
        fs::write(cache.path().join(name), b"raw").unwrap();
    }
    let photos = photos::corpus(&corpus, cache.path()).unwrap();
    assert_eq!(photos.len(), 5);
    assert_eq!(photos[0].camera, "Nikon Z 6");
    assert_eq!(photos[0].extension, "NEF");
    assert_eq!(photos[0].dimensions, [6064, 4040]);
    // The placeholder is not the pinned file, so a round refuses it.
    assert!(photos::verify(&photos[0]).is_err());
}

/// A small round over three targets and one RAW photograph: an exposure sweep, a radial mask and a
/// straighten on targets, exposure and white balance on the photograph.
fn reduced(original: &Path) -> Plan {
    let mut families = Vec::new();
    for mut family in rounds::round_one() {
        let keep = matches!(
            family.id.as_str(),
            "exposure" | "mask-radial" | "straighten" | "exposure-photographs" | "white-balance"
        );
        if keep {
            family.steps.truncate(2);
            family.subjects.retain(|subject| {
                matches!(
                    subject,
                    Subject::Target("wedge" | "flat" | "grid") | Subject::Photographs
                )
            });
            families.push(family);
        }
    }
    let bytes = fs::read(original).unwrap();
    Plan {
        round: 1,
        families,
        photos: vec![Photo {
            id: "pixls-1".into(),
            origin: "corpus",
            camera: "Test camera".into(),
            why: "a test".into(),
            path: original.to_path_buf(),
            extension: "NEF".into(),
            sha256: format!("{:x}", Sha256::digest(&bytes)),
            bytes: bytes.len() as u64,
            dimensions: [600, 400],
        }],
        excluded: vec![("own-air2s".into(), "a DNG".into())],
    }
}

/// Every file under `root`, by relative path, with its bytes.
fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(folder) = pending.pop() {
        for entry in fs::read_dir(&folder).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let relative = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                files.insert(relative, fs::read(&path).unwrap());
            }
        }
    }
    files
}

/// A round generated twice is the same folder byte for byte; it writes nothing outside its
/// folder and leaves the original as it was; every variant file's XMP, embedded or beside it,
/// reads back through the preset reader to the manifest's settings; and a failed run removes the
/// folder it made.
#[test]
#[slow]
fn a_round_is_deterministic_and_reads_back_from_its_files() {
    let directory = tempfile::tempdir().unwrap();
    let originals = directory.path().join("originals");
    fs::create_dir(&originals).unwrap();
    let original = originals.join("photo.NEF");
    fs::write(
        &original,
        b"not really a RAW file, only its bytes are copied",
    )
    .unwrap();
    let modified = fs::metadata(&original).unwrap().modified().unwrap();
    let plan = reduced(&original);
    let [first, second] = ["a", "b"].map(|name| directory.path().join(name));
    for parent in [&first, &second] {
        fs::create_dir(parent).unwrap();
        generate(&plan, &parent.join("round")).unwrap();
        assert_eq!(fs::read_dir(parent).unwrap().count(), 1);
    }
    let files = tree(&first.join("round"));
    assert_eq!(files, tree(&second.join("round")));
    assert_eq!(fs::read_dir(&originals).unwrap().count(), 1);
    assert_eq!(
        fs::metadata(&original).unwrap().modified().unwrap(),
        modified
    );

    let manifest: Value = serde_json::from_slice(&files["manifest.json"]).unwrap();
    let variants = manifest["variants"].as_array().unwrap();
    // Neutral and two steps of exposure on the wedge; neutral and two radial masks on the flat
    // field; neutral and two straightens on the grid; neutral, two exposures and two white
    // balances on the photograph.
    assert_eq!(variants.len(), 3 + 3 + 3 + 5);
    let images = files
        .keys()
        .filter(|path| path.starts_with("lightroom/"))
        .count();
    assert_eq!(images, 9 + 5 * 2);
    assert_eq!(manifest["not_included"].as_array().unwrap().len(), 4);
    let registry = ModuleRegistry::builtin();
    for variant in variants {
        let file = &files[variant["file"].as_str().unwrap()];
        let xmp = match variant["sidecar"].as_str() {
            Some(sidecar) => {
                assert_eq!(file.as_slice(), fs::read(&original).unwrap().as_slice());
                String::from_utf8(files[sidecar].clone()).unwrap()
            }
            None => embedded_xmp(file).unwrap(),
        };
        let read = luxforge_core::inspect_preset(&xmp, None, &registry).unwrap();
        let kind = if variant["sidecar"].is_null() {
            "jpeg"
        } else {
            "raw"
        };
        let baseline = manifest["reader_baseline"][kind].as_object().unwrap();
        assert_eq!(
            Value::Object(readback::changes(&read.settings, baseline)),
            variant["luxforge"],
            "{}",
            variant["id"]
        );
        // What the reader refuses is exactly what the manifest records as refused: a calibrated
        // value at its calibration's shortfall.
        let refused: Vec<&str> = read
            .report
            .refused
            .iter()
            .map(|r| r.setting.as_str())
            .collect();
        let recorded: Vec<&str> = variant["refused"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| r["setting"].as_str())
            .collect();
        assert_eq!(refused, recorded, "{}", variant["id"]);
    }
    // Exposure's slider follows Lightroom's response, so the reader transfers each value as it is.
    let exposure = |id: &str| variants.iter().find(|variant| variant["id"] == id).unwrap();
    assert_eq!(
        exposure("wedge__exposure__-5")["settings"],
        json!({"Exposure2012": "-5.00"})
    );
    assert!(luxforge_core::lightroom_calibration("Exposure2012", -5.0).is_none());
    for (id, value) in [("wedge__exposure__-5", -5.0), ("wedge__exposure__-3", -3.0)] {
        assert_eq!(
            exposure(id)["luxforge"],
            json!({"set-basic": {"exposure": value}})
        );
    }
    let mask = variants
        .iter()
        .find(|variant| variant["family"] == "mask-radial")
        .unwrap();
    assert_eq!(
        mask["unsupported"],
        json!(["CircularGradientBasedCorrections"])
    );

    // An existing folder, a missing parent and a changed original are refused; the last removes
    // the folder it began.
    let error = generate(&plan, &first.join("round"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("already exists"), "{error}");
    assert!(generate(&plan, &directory.path().join("missing/round")).is_err());
    let mut changed = reduced(&original);
    changed.photos[0].sha256 = "0".repeat(64);
    let failed = directory.path().join("failed");
    assert!(generate(&changed, &failed).is_err());
    assert!(!failed.exists());
}

/// `--families grain` selects round 1's three grain families by their area, and an unknown name
/// is refused. Their variants, written on the greys target, carry XMP that reads back through the
/// preset reader with Amount, Size and Roughness transferred at the same numbers onto `set-grain`,
/// nothing unsupported, and the size and roughness families holding Amount 50.
#[test]
#[slow]
fn grain_families_generate_and_read_back_as_mapped() {
    let families = select(1, Some(&["grain".to_owned()])).unwrap();
    let ids: Vec<&str> = families.iter().map(|family| family.id.as_str()).collect();
    assert_eq!(ids, ["grain-amount", "grain-size", "grain-roughness"]);
    let error = select(1, Some(&["grains".to_owned()]))
        .unwrap_err()
        .to_string();
    assert!(error.contains("no families or areas"), "{error}");
    let directory = tempfile::tempdir().unwrap();
    let round = directory.path().join("round");
    let families: Vec<Family> = families
        .into_iter()
        .map(|mut family| {
            family.subjects = vec![Subject::Target("greys")];
            family
        })
        .collect();
    generate(
        &Plan {
            round: 1,
            families,
            photos: Vec::new(),
            excluded: Vec::new(),
        },
        &round,
    )
    .unwrap();
    let manifest = read_json(&round.join("manifest.json")).unwrap();
    let variants = manifest["variants"].as_array().unwrap();
    assert_eq!(variants.len(), 1 + 5 + 6 + 6);
    let registry = ModuleRegistry::builtin();
    for variant in variants {
        let jpeg = fs::read(round.join(variant["file"].as_str().unwrap())).unwrap();
        let read =
            luxforge_core::inspect_preset(&embedded_xmp(&jpeg).unwrap(), None, &registry).unwrap();
        assert!(read.report.unsupported.is_empty(), "{}", variant["id"]);
        assert_eq!(variant["unsupported"], json!([]), "{}", variant["id"]);
        let expected = match variant["family"].as_str().unwrap() {
            NEUTRAL => json!({}),
            family => {
                // Whole numbers, as the reader reads the written integer texts.
                let value = number_of_label(variant["label"].as_str().unwrap());
                let (amount, size, roughness) = match family {
                    "grain-amount" => (value, 25, 50),
                    "grain-size" => (50, value, 50),
                    _ => (50, 25, value),
                };
                json!({"set-grain": {"amount": amount, "size": size, "roughness": roughness}})
            }
        };
        assert_eq!(variant["luxforge"], expected, "{}", variant["id"]);
        assert_eq!(
            Value::Object(readback::changes(
                &read.settings,
                manifest["reader_baseline"]["jpeg"].as_object().unwrap()
            )),
            expected,
            "{}",
            variant["id"]
        );
    }
}

fn number_of_label(label: &str) -> i64 {
    label.trim_start_matches('+').parse().unwrap()
}

/// `--round` names a round the rig defines.
#[test]
fn an_unknown_round_is_refused() {
    let directory = tempfile::tempdir().unwrap();
    let args = |round: &str| {
        Args(
            ["generate", "--round", round, "--output"]
                .into_iter()
                .map(Into::into)
                .chain([directory.path().join("round").into_os_string()])
                .collect(),
        )
    };
    let error = run(directory.path(), args("4")).unwrap_err().to_string();
    assert!(error.contains("rounds 1, 2 and 3"), "{error}");
    assert!(!directory.path().join("round").exists());
}
