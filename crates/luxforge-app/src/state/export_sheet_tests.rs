use super::*;
use luxforge_core::AssetId;
use serde_json::json;

fn web() -> ExportPreset {
    ExportPreset {
        preset_id: WEB.into(),
        name: "Web".into(),
        builtin: true,
        settings: ExportSettings {
            format: FormatSettings::Jpeg {
                quality: 80,
                chroma: Chroma::Half,
                limit_bytes: None,
            },
            size: OutputSize::LongEdge { pixels: 2048 },
            optimise: true,
            sharpen: Some(Sharpen {
                medium: SharpenMedium::Screen,
                amount: SharpenAmount::Standard,
            }),
            metadata: MetadataLevel::Copyright,
            pixels_per_inch: None,
            folder: ExportFolder::Original,
            suffix: "-web".into(),
        },
    }
}

fn mine() -> ExportPreset {
    ExportPreset {
        preset_id: "mine".into(),
        name: "Instagram".into(),
        builtin: false,
        settings: ExportSettings {
            size: OutputSize::Within {
                width: 1080,
                height: 1350,
            },
            ..web().settings
        },
    }
}

fn single() -> SheetTarget {
    SheetTarget::Single {
        asset_id: AssetId::new(),
        entry_id: EntryId::new(),
        file_name: "DSC_0042.NEF".into(),
        stem: "DSC_0042".into(),
    }
}

fn sheet() -> ExportSheet {
    let web = web();
    ExportSheet::new(single(), false, web.settings, Some(web.preset_id))
}

/// The sheet with its plan answered for its current settings.
fn answered(mut sheet: ExportSheet, plan: Value) -> ExportSheet {
    sheet.plan.answered = Some(sheet.settings.clone());
    sheet.plan.asked = Some(sheet.settings.clone());
    sheet.plan.answer = Some(SheetPlan::Single(plan));
    sheet
}

fn plan(notices: Value, suggested: Value) -> Value {
    json!({
        "stage": {"width": 6048, "height": 4024},
        "output": {"width": 2048, "height": 1363},
        "suggested": suggested,
        "stem": "DSC_0042",
        "name": "DSC_0042-web.jpg",
        "estimate": {"bytes": 1},
        "notices": notices,
    })
}

#[test]
fn only_a_setting_the_plan_depends_on_asks_for_a_plan() {
    let mut sheet = sheet();
    assert!(
        sheet.wants_plan().is_some(),
        "a new sheet asks for its first plan"
    );
    sheet.plan.answered = Some(sheet.settings.clone());
    sheet.plan.asked = Some(sheet.settings.clone());
    assert_eq!(sheet.wants_plan(), None);
    // The quality slider and the metadata level never ask.
    sheet.set_quality(55);
    sheet.set_metadata(MetadataLevel::All);
    assert_eq!(sheet.wants_plan(), None);
    // Each of format, size, density, folder and suffix does.
    let changes: [fn(&mut ExportSheet); 5] = [
        |sheet| sheet.set_format(ExportFormat::Webp),
        |sheet| sheet.set_fit(Fit::Full),
        |sheet| sheet.set_density(true),
        |sheet| sheet.set_folder("/tmp/out".into()),
        |sheet| sheet.type_text(Field::Suffix, "-x".into()),
    ];
    for change in changes {
        let mut changed = sheet.clone();
        change(&mut changed);
        assert!(changed.wants_plan().is_some(), "{:?}", changed.settings);
    }
}

#[test]
fn a_typed_field_that_reads_as_no_value_keeps_the_settings_and_says_why() {
    let mut sheet = sheet();
    let before = sheet.settings.clone();
    sheet.type_text(Field::LongEdge, "big".into());
    assert_eq!(sheet.settings, before);
    assert_eq!(
        sheet.invalid,
        Some((Field::LongEdge, "a whole number of pixels".into()))
    );
    assert_eq!(sheet.valid(), None);
    assert_eq!(
        sheet.wants_plan(),
        None,
        "an invalid sheet asks for nothing"
    );
    sheet.type_text(Field::LongEdge, "3000".into());
    assert_eq!(sheet.invalid, None);
    assert_eq!(sheet.settings.size, OutputSize::LongEdge { pixels: 3000 });
}

#[test]
fn a_format_chosen_back_restores_its_own_choices() {
    let mut sheet = sheet();
    sheet.set_quality(62);
    sheet.set_format(ExportFormat::Tiff);
    sheet.set_bit_depth(Depth::Eight);
    assert_eq!(sheet.settings.quality(), None);
    sheet.set_format(ExportFormat::Jpeg);
    assert_eq!(sheet.settings.quality(), Some(62));
    assert_eq!(sheet.settings.chroma(), Some(Chroma::Half));
    sheet.set_format(ExportFormat::Tiff);
    assert_eq!(
        sheet.settings.format,
        FormatSettings::Tiff {
            bit_depth: Depth::Eight
        }
    );
}

#[test]
fn a_change_marks_the_card_and_is_remembered_without_its_preset() {
    let presets = [web(), mine()];
    let mut sheet = sheet();
    assert!(!sheet.changed(&presets));
    assert_eq!(sheet.remembered_preset(&presets), Some(WEB.into()));
    sheet.set_quality(60);
    assert!(sheet.changed(&presets));
    assert_eq!(sheet.remembered_preset(&presets), None);
    let shown = model(Some(&sheet), Some(&presets), None).unwrap();
    assert_eq!(shown.cards[0].detail, "Changed from preset");
    assert!(shown.cards[0].changed);
    // A built-in preset offers Save as preset…, never Update.
    assert_eq!(shown.update, None);
    sheet.choose(&presets[1]);
    sheet.set_quality(70);
    let shown = model(Some(&sheet), Some(&presets), None).unwrap();
    assert_eq!(
        shown.update.as_deref(),
        Some("Update \u{201c}Instagram\u{201d}\u{2026}")
    );
    assert_eq!(shown.cards[1].detail, "Changed from preset");
    assert_eq!(shown.cards[0].detail, "JPEG \u{b7} 2048 px");
}

#[test]
fn a_card_menu_offers_duplicate_alone_on_a_built_in_preset() {
    let presets = [web(), mine()];
    let mut sheet = sheet();
    sheet.card_menu = Some(WEB.into());
    let shown = model(Some(&sheet), Some(&presets), None).unwrap();
    assert_eq!(shown.cards[0].menu, Some(vec![CardChoice::Duplicate]));
    sheet.card_menu = Some("mine".into());
    let shown = model(Some(&sheet), Some(&presets), None).unwrap();
    assert_eq!(
        shown.cards[1].menu,
        Some(vec![
            CardChoice::Update,
            CardChoice::Rename,
            CardChoice::Duplicate,
            CardChoice::Delete
        ])
    );
}

#[test]
fn the_large_image_notice_shows_exactly_when_the_plan_carries_it() {
    let presets = [web()];
    let quiet = answered(sheet(), plan(json!([]), json!("/p/DSC_0042-web.jpg")));
    let model_quiet = model(Some(&quiet), Some(&presets), None).unwrap();
    assert_eq!(model_quiet.large_image, None);
    let large = answered(
        sheet(),
        plan(
            json!([{"code": "large-image", "refuses": false, "bytes": 720_000_000,
                "message": "Large image: optimising 60.2 MP holds about 720 MB while encoding and is several times slower"}]),
            json!("/p/DSC_0042-web.jpg"),
        ),
    );
    let model_large = model(Some(&large), Some(&presets), None).unwrap();
    assert_eq!(
        model_large.large_image.as_deref(),
        Some(
            "Large image: optimising 60.2 MP holds about 720 MB while encoding and is several \
             times slower. Turn Optimise off to write it as it renders."
        )
    );
    // It warns; it never blocks.
    assert!(model_large.can_export);
}

/// A merge's result is named from the stem its plan answers, its first input's with the kind's
/// suffix; a plan that names no file and refuses nothing says why Export is off.
#[test]
fn the_plans_stem_names_the_file_and_a_plan_naming_none_says_why() {
    let presets = [web()];
    let mut merged = plan(json!([]), json!("/p/LXF_0005-pano-web.jpg"));
    merged["stem"] = json!("LXF_0005-pano");
    merged["name"] = json!("LXF_0005-pano-web.jpg");
    let shown = model(Some(&answered(sheet(), merged)), Some(&presets), None).unwrap();
    assert_eq!(shown.stem, "LXF_0005-pano");
    assert_eq!(shown.taken, None);
    assert!(shown.can_export && shown.refusals.is_empty());
    let mut nameless = plan(json!([]), Value::Null);
    nameless["stem"] = Value::Null;
    nameless["name"] = Value::Null;
    let shown = model(Some(&answered(sheet(), nameless)), Some(&presets), None).unwrap();
    assert!(!shown.can_export);
    assert_eq!(
        shown.refusals,
        ["This photograph has no file name to export under"]
    );
    let full = model(
        Some(&answered(sheet(), plan(json!([]), Value::Null))),
        Some(&presets),
        None,
    )
    .unwrap();
    assert!(!full.can_export);
    assert_eq!(
        full.refusals,
        [
            "DSC_0042-web.jpg and the next names after it are all taken in this folder; choose \
          another folder or suffix"
        ]
    );
}

#[test]
fn a_taken_name_is_reported_with_the_name_written_instead() {
    let presets = [web()];
    let sheet = answered(sheet(), plan(json!([]), json!("/p/DSC_0042-web-2.jpg")));
    let shown = model(Some(&sheet), Some(&presets), None).unwrap();
    assert_eq!(shown.name.as_deref(), Some("\u{2192} DSC_0042-web-2.jpg"));
    assert_eq!(
        shown.taken.as_deref(),
        Some("DSC_0042-web.jpg exists there; this writes DSC_0042-web-2.jpg")
    );
    assert_eq!(
        shown.readout,
        "2048 \u{d7} 1363 px \u{b7} 2.8 MP \u{b7} 34% of 6048 \u{d7} 4024"
    );
}

#[test]
fn a_refusing_notice_or_a_stale_plan_disables_export() {
    let presets = [web()];
    let refused = answered(
        sheet(),
        plan(
            json!([{"code": "webp-too-large", "refuses": true,
                "message": "WebP is limited to 16383 px a side"}]),
            json!(null),
        ),
    );
    let model_refused = model(Some(&refused), Some(&presets), None).unwrap();
    assert_eq!(
        model_refused.refusals,
        vec!["WebP is limited to 16383 px a side"]
    );
    assert!(!model_refused.can_export);
    // A change the plan has not answered yet holds Export until it does; quality does not.
    let mut stale = answered(sheet(), plan(json!([]), json!("/p/a-web.jpg")));
    stale.set_quality(40);
    assert!(
        model(Some(&stale), Some(&presets), None)
            .unwrap()
            .can_export
    );
    stale.set_fit(Fit::Full);
    assert!(
        !model(Some(&stale), Some(&presets), None)
            .unwrap()
            .can_export
    );
}

#[test]
fn a_batch_sheet_counts_its_photographs_and_the_ones_it_will_leave_out() {
    let presets = [web()];
    let mut sheet = ExportSheet::new(
        SheetTarget::Batch {
            count: 5,
            raw: 4,
            jpeg: 1,
            assets: Vec::new(),
        },
        false,
        web().settings,
        Some(WEB.into()),
    );
    let mut batch = BatchPlan::default();
    let large = json!({"suggested": "/a/DSC_0001-web.jpg", "notices": [
        {"code": "large-image", "refuses": false, "bytes": 400_000_000, "message": "m"}]});
    batch.count(Ok(&large));
    batch.count(Ok(
        &json!({"suggested": "/a/DSC_0002-web.jpg", "notices": []}),
    ));
    batch.count(Err("the original is missing"));
    sheet.plan.answered = Some(sheet.settings.clone());
    sheet.plan.asked = Some(sheet.settings.clone());
    sheet.plan.answer = Some(SheetPlan::Batch(batch));
    let shown = model(Some(&sheet), Some(&presets), None).unwrap();
    assert_eq!(shown.title, "Export 5 photos");
    assert_eq!(shown.subtitle, "4 RAW \u{b7} 1 JPEG \u{b7} current edits");
    assert_eq!(
        shown.export_label, "Export 4",
        "the photograph left out is not counted"
    );
    assert_eq!(shown.name.as_deref(), Some("DSC_0001-web.jpg, \u{2026}"));
    assert_eq!(
        shown.large_image.as_deref(),
        Some(
            "Large images: 1 photo over 24 MP hold up to about 400 MB each while encoding and \
             are several times slower."
        )
    );
    assert_eq!(
        shown.refusals,
        vec!["1 photo could not be planned: The original is missing"]
    );
    assert!(shown.can_export, "one left out does not hold the rest");
    assert_eq!(
        shown.summary,
        "4 files \u{b7} 1 left out \u{b7} JPEG 80 \u{b7} optimised \u{b7} long edge 2048 \u{b7} sRGB"
    );
}

#[test]
fn a_batch_that_can_write_nothing_cannot_export() {
    let presets = [web()];
    let mut sheet = ExportSheet::new(
        SheetTarget::Batch {
            count: 2,
            raw: 0,
            jpeg: 2,
            assets: Vec::new(),
        },
        false,
        web().settings,
        Some(WEB.into()),
    );
    let refused = |code: &str| json!({"code": code, "refuses": true, "message": "m"});
    let mut batch = BatchPlan::default();
    // Two refusing notices leave one photograph out once.
    batch.count(Ok(&json!({"suggested": null, "notices": [
        refused("webp-too-large"), refused("folder-missing")]})));
    batch.count(Ok(
        &json!({"suggested": null, "notices": [refused("webp-too-large")]}),
    ));
    assert_eq!(batch.left_out, 2);
    sheet.plan.answered = Some(sheet.settings.clone());
    sheet.plan.asked = Some(sheet.settings.clone());
    sheet.plan.answer = Some(SheetPlan::Batch(batch));
    let shown = model(Some(&sheet), Some(&presets), None).unwrap();
    assert_eq!(shown.export_label, "Export 0");
    assert!(
        shown
            .summary
            .starts_with("0 files \u{b7} 2 left out \u{b7} ")
    );
    assert!(!shown.can_export);
}

#[test]
fn a_card_names_its_format_size_and_density() {
    let mut settings = web().settings;
    assert_eq!(card_detail(&settings), "JPEG \u{b7} 2048 px");
    settings.format = FormatSettings::Tiff {
        bit_depth: Depth::Sixteen,
    };
    settings.size = OutputSize::Full;
    settings.pixels_per_inch = Some(300);
    assert_eq!(
        card_detail(&settings),
        "TIFF \u{b7} 16-bit \u{b7} full \u{b7} 300 ppi"
    );
}

#[test]
fn the_sharpen_row_chooses_a_medium_and_amount_and_marks_a_change_from_the_preset() {
    let presets = [web()];
    let mut sheet = answered(sheet(), plan(json!([]), json!("/p/DSC_0042-web.jpg")));
    let shown = model(Some(&sheet), Some(&presets), None).unwrap().sharpen;
    assert_eq!(
        shown,
        SharpenModel {
            medium: 1,
            amount: 1,
            amount_enabled: true,
            changed: None,
        },
        "Web sharpens for the screen at Standard"
    );
    // Matte, then High: the card and the row are marked, and no plan is asked for.
    sheet.set_sharpen(Some(SharpenMedium::Matte));
    sheet.set_sharpen_amount(SharpenAmount::High);
    assert_eq!(
        sheet.settings.sharpen,
        Some(Sharpen {
            medium: SharpenMedium::Matte,
            amount: SharpenAmount::High,
        })
    );
    assert!(sheet.changed(&presets));
    assert!(sheet.wants_plan().is_none(), "sharpening changes no plan");
    let shown = model(Some(&sheet), Some(&presets), None).unwrap();
    assert_eq!(
        shown.sharpen.changed.as_deref(),
        Some("Changed from Web (Screen, Standard)")
    );
    assert_eq!((shown.sharpen.medium, shown.sharpen.amount), (2, 2));
    assert!(shown.cards[0].changed);
    // None disables the amount and keeps it for the next medium.
    sheet.set_sharpen(None);
    let shown = model(Some(&sheet), Some(&presets), None).unwrap().sharpen;
    assert_eq!(
        (shown.medium, shown.amount, shown.amount_enabled),
        (0, 2, false)
    );
    sheet.set_sharpen_amount(SharpenAmount::Low);
    assert_eq!(sheet.settings.sharpen, None);
    sheet.set_sharpen(Some(SharpenMedium::Glossy));
    assert_eq!(
        sheet.settings.sharpen,
        Some(Sharpen {
            medium: SharpenMedium::Glossy,
            amount: SharpenAmount::Low,
        })
    );
    // Back to the preset's own: nothing is marked.
    sheet.set_sharpen(Some(SharpenMedium::Screen));
    sheet.set_sharpen_amount(SharpenAmount::Standard);
    assert!(!sheet.changed(&presets));
    let shown = model(Some(&sheet), Some(&presets), None).unwrap().sharpen;
    assert_eq!(shown.changed, None);
}
