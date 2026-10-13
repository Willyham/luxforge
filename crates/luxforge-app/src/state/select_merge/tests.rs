//! Select's merge state as plain data: the requests it sends, the words it says and the states the
//! merge preview sheet draws, against hand-made owner answers.
use super::sheet::{
    self, EV_STEP, Picture, PreviewSheet, SheetContext, Tint, files_words, fit, hatch,
    shutter_words, size_words, stepped, thirds_words,
};
use super::*;
use luxforge_core::{
    catalog_types::{
        FileId, FolderProposal, HdrParameters, HdrPreviewDiagnostics, HdrPreviewOverlays,
        MergeEstimates, MergeOptions as WireOptions, MergeOutput, MergeParameters, MergePreview,
        MergePreviewDiagnostics, MergePreviewOverlays, PlannedInput, ProviderState,
    },
    merge::hdr::{
        ExposureOrigin, ExposureSettings, FrameMetadata, MotionOverlay, PixelRect, Recording,
        ReferenceOrigin, ResolvedExposure, ResolvedExposures,
    },
};

fn request() -> MergeRequest {
    MergeRequest {
        kind: MergeKind::Hdr,
        targets: Targets::Files {
            file_ids: vec![FileId(1), FileId(2), FileId(3)],
        },
        options: MergeOptions::default(),
        source: None,
        anchor: 0,
        frames: 3,
    }
}

fn mutation() -> MutationRequest {
    MutationRequest {
        request_id: "r".into(),
        actor: "desktop".into(),
    }
}

const OFFSETS: [f64; 3] = [-2.0, 0.0, 2.0];

/// A plan of the three frames DSC_4088–4090.NEF, the middle one the reference, into Konstanz.
fn plan() -> MergePlan {
    const SHUTTERS: [f64; 3] = [1.0 / 1000.0, 1.0 / 250.0, 1.0 / 60.0];
    let inputs = (0..3)
        .map(|at| PlannedInput {
            ordinal: at as u32,
            path: PathBuf::from(format!("/cards/DSC_{}.NEF", 4088 + at)),
            file_name: format!("DSC_{}.NEF", 4088 + at),
            file_id: Some(FileId(at as i64 + 1)),
            asset_id: None,
            available: true,
            frame: FrameMetadata {
                identity: format!("frame-{at}"),
                make: Some("NIKON CORPORATION".into()),
                model: Some("NIKON Z 8".into()),
                recording: Recording::Jpeg,
                width: 6048,
                height: 4024,
                orientation: 1,
                exposure: ExposureSettings {
                    shutter_s: Some(SHUTTERS[at]),
                    ..ExposureSettings::default()
                },
                captured_at_ms: None,
            },
        })
        .collect();
    MergePlan {
        kind: MergeKind::Hdr,
        plan_id: "plan-1".into(),
        inputs,
        suitable: true,
        refusal: None,
        exposures: Some(ResolvedExposures {
            reference: 1,
            reference_origin: ReferenceOrigin::ZeroBias,
            inputs: OFFSETS
                .iter()
                .map(|offset| ResolvedExposure {
                    offset_ev: *offset,
                    scale: (-offset).exp2(),
                    origin: ExposureOrigin::Reference,
                })
                .collect(),
        }),
        needs_fit: false,
        reference: Some(1),
        parameters: MergeParameters::Hdr(HdrParameters::default()),
        folder: Some(FolderProposal {
            folder_id: CatalogFolderId::new(),
            name: "Konstanz".into(),
            from: "existing".into(),
        }),
        suggested_folder: None,
        estimates: MergeEstimates {
            width: Some(6048),
            height: Some(4024),
            temp_bytes: None,
            source_bytes: Some(300 * 1024 * 1024),
            available_bytes: None,
            temp_ceiling_bytes: 1,
            scratch_limit_bytes: 1,
            scale: None,
            provisional: false,
        },
        provider: ProviderState {
            module: "luxforge.hdr".into(),
            version: 1,
            available: true,
            reason: None,
        },
        existing: None,
        claimed: Vec::new(),
    }
}

fn preview_id() -> MergePreviewId {
    serde_json::from_value(json!("merge-preview-0123456789abcdef0123456789abcdef")).unwrap()
}

/// A ready revision: 1,024-pixel frames aligned to 0.04 px, highlights extended, one moving region
/// from the reference and one fallback from −2 EV.
fn revision(revision: u32) -> MergePreview {
    MergePreview {
        preview_id: preview_id(),
        revision,
        kind: MergeKind::Hdr,
        options: WireOptions::Hdr(Default::default()),
        path: Some(PathBuf::from(format!(
            "/cache/merge-preview-{revision}.jpg"
        ))),
        width: 1024,
        height: 681,
        diagnostics: Some(MergePreviewDiagnostics::Hdr(HdrPreviewDiagnostics {
            proxy_side: 1024,
            alignment: AlignmentMode::Auto,
            max_residual_px: Some(0.04),
            deghost: DeghostMode::Auto,
            highlights_extended: true,
            shadows_extended: false,
            exposures: OFFSETS.to_vec(),
            reference: 1,
        })),
        overlays: Some(MergePreviewOverlays::Hdr(HdrPreviewOverlays {
            motion: vec![
                MotionOverlay {
                    input: 1,
                    offset_ev: 0.0,
                    fallback: false,
                    bounds: PixelRect {
                        x: 20,
                        y: 300,
                        width: 160,
                        height: 50,
                    },
                    outline: vec![[20.0, 300.0], [180.0, 300.0], [180.0, 350.0], [20.0, 350.0]],
                },
                MotionOverlay {
                    input: 0,
                    offset_ev: -2.0,
                    fallback: true,
                    bounds: PixelRect {
                        x: 600,
                        y: 40,
                        width: 80,
                        height: 80,
                    },
                    outline: vec![[600.0, 40.0], [680.0, 40.0], [640.0, 120.0]],
                },
            ],
        })),
        output: Some(MergeOutput {
            width: 6048,
            height: 4024,
            scale: 1.0,
            source_bytes: 279 * 1024 * 1024,
        }),
        refusal: None,
    }
}

fn open_sheet() -> PreviewSheet {
    PreviewSheet::new(
        request(),
        SheetContext {
            names: vec!["DSC_4088.NEF".into(), "DSC_4090.NEF".into()],
            camera: Some("Nikon Z 8".into()),
            capture: Some("13 Sep 2026 17:31".into()),
        },
        Vec::new(),
    )
}

/// The defaults send nothing but the frames and the folder choice; every option changed is sent
/// as an agent writes it.
#[test]
fn hdr_merge_requests_send_only_what_was_chosen() {
    let folder = FolderChoice::Place {
        name: "Konstanz".into(),
    };
    let defaults = request();
    assert_eq!(
        plan_params(defaults.kind, &defaults.targets, &defaults.options),
        json!({"kind": "hdr", "targets": {"kind": "files", "file_ids": [1, 2, 3]}})
    );
    assert_eq!(
        create_params(
            &defaults,
            CreateFrom::Plan("plan"),
            Some(&folder),
            &mutation()
        ),
        json!({
            "kind": "hdr",
            "targets": {"kind": "files", "file_ids": [1, 2, 3]},
            "plan_id": "plan",
            "folder": {"kind": "place", "name": "Konstanz"},
            "mutation": {"request_id": "r", "actor": "desktop"},
        })
    );
    // With no folder to send, the core's default stands.
    assert!(
        create_params(&defaults, CreateFrom::Plan("plan"), None, &mutation())
            .get("folder")
            .is_none()
    );
    let mut chosen = request();
    chosen.options = MergeOptions {
        alignment: Toggle::Off,
        deghost: Toggle::Off,
        reference: Some("/a/2.NEF".into()),
        overrides: vec![("/a/1.NEF".into(), -2.0)],
        folder: FolderPick::Proposed,
        ..MergeOptions::default()
    };
    let params = create_params(
        &chosen,
        CreateFrom::Plan("plan"),
        Some(&folder),
        &mutation(),
    );
    assert_eq!(params["kind"], "hdr");
    assert_eq!(
        params["hdr"],
        json!({
            "alignment": "off",
            "deghost": "off",
            "reference": "/a/2.NEF",
            "exposure_overrides": [{"path": "/a/1.NEF", "offset_ev": -2.0}],
        })
    );
    for moved in ["alignment", "deghost", "reference", "exposure_overrides"] {
        assert!(params.get(moved).is_none(), "{moved} is under hdr");
    }
    for removed in ["folder_id", "add_to_development_set"] {
        assert!(params.get(removed).is_none(), "{removed} is not sent");
    }
}

/// The folder choice sent: the plan's existing folder, else the choice it suggests, whose folders
/// the merge makes; a folder the person chose is sent as itself, with no day folder.
#[test]
fn hdr_merge_sends_the_plans_folder_choice_or_the_persons() {
    let mut planned = plan();
    let proposed = planned.folder.clone().unwrap().folder_id;
    let options = MergeOptions::default();
    assert_eq!(
        folder_choice(&options, &planned),
        Some(FolderChoice::Existing {
            folder_id: proposed,
            days: false,
        })
    );
    planned.folder = None;
    planned.suggested_folder = Some(FolderChoice::Place {
        name: "Konstanz".into(),
    });
    assert_eq!(
        folder_choice(&options, &planned),
        Some(FolderChoice::Place {
            name: "Konstanz".into()
        })
    );
    let chosen = CatalogFolderId::new();
    let options = MergeOptions {
        folder: FolderPick::Existing {
            folder_id: chosen.clone(),
            name: "2026 \u{203a} Lake".into(),
        },
        ..MergeOptions::default()
    };
    assert_eq!(
        folder_choice(&options, &planned),
        Some(FolderChoice::Existing {
            folder_id: chosen,
            days: false,
        })
    );
    planned.suggested_folder = None;
    assert_eq!(folder_choice(&MergeOptions::default(), &planned), None);
}

#[test]
fn hdr_merge_words_are_made_once_and_name_the_input_or_stage() {
    assert_eq!(
        stack_words(MergeKind::Hdr, 3),
        ("HDR \u{b7} 3 exposures", "HDR \u{b7} 3")
    );
    assert_eq!(stack_words(MergeKind::Hdr, 40), ("HDR", "HDR"));
    assert_eq!(
        words(MergeKind::Hdr).hint,
        "Merge the frames into one HDR photograph in the catalog"
    );
    assert_eq!(words(MergeKind::Panorama).action, "Merge panorama");
    let mut pending = PendingMerge::new("job".into(), request());
    assert_eq!(pending.title(), "Merging HDR \u{b7} 3 exposures");
    pending.failed = Some("no".into());
    assert_eq!(pending.title(), "HDR merge failed \u{b7} 3 exposures");
    assert_eq!(ev_words(-2.0), Some("\u{2212}2 EV"));
    assert_eq!(ev_words(0.0), Some("0 EV"));
    assert_eq!(ev_words(0.123), None);
    let failed = json!({
        "status": "failed",
        "error": {"code": "unsupported-input", "message": "it could not be developed",
                  "data": {"path": "/x/DSC_0002.NEF"}},
    });
    assert_eq!(
        failure_text(&failed),
        "DSC_0002.NEF: it could not be developed"
    );
    let staged = json!({
        "status": "failed",
        "error": {"code": "internal", "message": "out of memory"},
        "result": {"stage": {"stage": "aligning"}},
    });
    assert_eq!(failure_text(&staged), "While aligning: out of memory");
    assert_eq!(stack_name(Some(MergeKind::Hdr)), "an HDR stack");
    assert_eq!(stack_name(Some(MergeKind::Panorama)), "a panorama stack");
    assert_eq!(stack_name(None), "a merge stack");
}

/// The preview's requests: `merge.preview` of the frames and their plan with the options chosen,
/// every update the whole options under `hdr` (an empty object for the defaults), the close by
/// identity, and a merge from it naming the preview and no plan.
#[test]
fn merge_preview_requests_name_the_preview_and_its_whole_options() {
    let mut merging = request();
    assert_eq!(
        preview_params(&merging, Some("plan-1")),
        json!({
            "kind": "hdr",
            "targets": {"kind": "files", "file_ids": [1, 2, 3]},
            "plan_id": "plan-1",
        })
    );
    let id = preview_id();
    assert_eq!(
        preview_update_params(MergeKind::Hdr, &id, &merging.options),
        json!({"preview_id": "merge-preview-0123456789abcdef0123456789abcdef", "hdr": {}})
    );
    merging.options.deghost = Toggle::Off;
    merging
        .options
        .set_override("/cards/DSC_4088.NEF".into(), Some(-5.0 / 3.0));
    let update = preview_update_params(MergeKind::Hdr, &id, &merging.options);
    assert_eq!(
        update,
        json!({
            "preview_id": "merge-preview-0123456789abcdef0123456789abcdef",
            "hdr": {
                "deghost": "off",
                "exposure_overrides": [{"path": "/cards/DSC_4088.NEF", "offset_ev": -5.0 / 3.0}],
            },
        })
    );
    assert_eq!(
        preview_close_params(&id),
        json!({"preview_id": "merge-preview-0123456789abcdef0123456789abcdef"})
    );
    let folder = FolderChoice::Existing {
        folder_id: CatalogFolderId::new(),
        days: false,
    };
    let create = create_params(
        &merging,
        CreateFrom::Preview(&id),
        Some(&folder),
        &mutation(),
    );
    assert_eq!(
        create["preview_id"],
        "merge-preview-0123456789abcdef0123456789abcdef"
    );
    assert!(create.get("plan_id").is_none());
    assert_eq!(create["hdr"], update["hdr"], "the options the preview drew");
    assert_eq!(create["folder"], json!(folder));
}

/// ⌥-click's settings: the defaults with nothing remembered, else the remembered alignment and
/// deghosting, sent as an agent writes them; what a merge stores back is the same pair.
#[test]
fn merge_preview_last_settings_come_from_the_remembered_state() {
    let defaults = MergeOptions::last(None, None);
    assert_eq!(defaults, MergeOptions::default());
    assert!(defaults.wire(MergeKind::Hdr).is_empty());
    let remembered = HdrMergePreference {
        alignment: AlignmentMode::Off,
        deghost: DeghostMode::Auto,
    };
    let last = MergeOptions::last(Some(remembered), None);
    assert_eq!((last.alignment, last.deghost), (Toggle::Off, Toggle::Auto));
    assert_eq!(
        Value::Object(last.wire(MergeKind::Hdr)),
        json!({"alignment": "off"})
    );
    assert_eq!(last.remembered(), remembered);
    assert_eq!(
        MergeOptions::default().remembered(),
        HdrMergePreference::default()
    );
}

/// A selection's merge menu, of 3 frames: the HDR group and the Panorama group, each headed by its
/// kind with its one item and hint and the ellipsis that ⌥ drops; a plan's refusal disables only
/// its own kind's item with its reason, and a merge already running disables both.
#[test]
fn merge_preview_menu_offers_each_kind_and_option_drops_the_ellipsis() {
    let offers = |hdr: Option<&str>, panorama: Option<&str>| {
        vec![
            KindOffer {
                kind: MergeKind::Hdr,
                refusal: hdr.map(str::to_owned),
            },
            KindOffer {
                kind: MergeKind::Panorama,
                refusal: panorama.map(str::to_owned),
            },
        ]
    };
    let frames = |count, kinds| {
        Some(MergeMenu {
            x: 0.0,
            y: 0.0,
            kind: MergeMenuKind::Frames { count, kinds },
        })
    };
    let mut merge = SelectMerge {
        menu: frames(3, offers(None, None)),
        ..SelectMerge::default()
    };
    let menu = derive(&merge).menu.unwrap();
    let headings: Vec<_> = menu.groups.iter().map(|group| group.heading).collect();
    assert_eq!(headings, [Some("HDR"), Some("Panorama")]);
    let hdr = menu.group("HDR").unwrap();
    let labels: Vec<_> = hdr
        .entries
        .iter()
        .map(|entry| entry.label.as_str())
        .collect();
    assert_eq!(labels, ["Merge HDR\u{2026}"]);
    assert_eq!(
        hdr.entries[0].hint,
        Some("Merge the frames into one HDR photograph in the catalog")
    );
    assert_eq!(
        hdr.entries[0].action,
        Some(MergeAction::Start(MergeKind::Hdr))
    );
    let panorama = menu.group("Panorama").unwrap();
    assert_eq!(panorama.entries.len(), 1);
    assert_eq!(panorama.entries[0].label, "Merge panorama\u{2026}");
    assert_eq!(
        panorama.entries[0].hint,
        Some("Merge the frames into one panorama in the catalog")
    );
    assert_eq!(
        panorama.entries[0].action,
        Some(MergeAction::Start(MergeKind::Panorama))
    );
    merge.option = true;
    let menu = derive(&merge).menu.unwrap();
    assert_eq!(menu.group("HDR").unwrap().entries[0].label, "Merge HDR");
    assert_eq!(
        menu.group("Panorama").unwrap().entries[0].label,
        "Merge panorama"
    );

    let reason = "Exposures must differ: DSC_4102.NEF and DSC_4103.NEF are both 0 EV";
    merge.menu = frames(7, offers(Some(reason), None));
    let menu = derive(&merge).menu.unwrap();
    let hdr = &menu.group("HDR").unwrap().entries[0];
    assert!(hdr.action.is_none());
    assert_eq!(hdr.reason.as_deref(), Some(reason));
    let panorama = &menu.group("Panorama").unwrap().entries[0];
    assert_eq!(
        panorama.action,
        Some(MergeAction::Start(MergeKind::Panorama))
    );
    assert_eq!(panorama.reason, None);

    merge.menu = frames(3, offers(None, None));
    merge.starting = Some(request());
    for group in derive(&merge).menu.unwrap().groups {
        for entry in group.entries {
            assert!(entry.action.is_none());
            assert_eq!(entry.reason.as_deref(), Some(BUSY));
        }
    }
}

/// Each kind is offered only within its input bounds: HDR's 2–9, a panorama's 2–32.
#[test]
fn panorama_merge_menu_offers_each_kind_within_its_bounds() {
    assert!(offered(1).is_empty());
    assert_eq!(offered(2), [MergeKind::Hdr, MergeKind::Panorama]);
    assert_eq!(offered(9), [MergeKind::Hdr, MergeKind::Panorama]);
    assert_eq!(offered(10), [MergeKind::Panorama]);
    assert_eq!(offered(32), [MergeKind::Panorama]);
    assert!(offered(33).is_empty());
}

/// The sheet's states in turn: reading the frames, preparing with the job's words and bar while
/// the merges stay live, the ready picture with its status line, output and frames, the overlay
/// drawn only while shown, a refusal that replaces the picture and disables Merge, and a merge
/// running elsewhere disabling it with its reason.
#[test]
fn merge_preview_sheet_draws_each_state_from_the_owner_answers() {
    let mut sheet = open_sheet();
    let model = sheet::model(&sheet, None);
    assert_eq!(model.title, "Merge HDR \u{b7} 3 exposures");
    assert_eq!(
        model.subtitle,
        "DSC_4088\u{2013}4090.NEF \u{b7} Nikon Z 8 \u{b7} 13 Sep 2026 17:31"
    );
    assert!(model.reading && model.frames.is_empty());

    // Planned, then preparing: the job's words and fraction, settings and merges live.
    sheet.planned(plan());
    sheet.following("job-1".into(), None);
    sheet.observe(&json!({
        "status": "running",
        "progress": {"fraction": 0.4, "message": "Reading 2 of 3"},
        "result": {"stage": "reading", "preview_id": "merge-preview-0123456789abcdef0123456789abcdef"},
    }));
    assert_eq!(sheet.preview_id, Some(preview_id()));
    let model = sheet::model(&sheet, None);
    assert_eq!(
        model.picture,
        Picture::Preparing {
            message: "Reading 2 of 3".into(),
            fraction: Some(0.4),
        }
    );
    assert_eq!(model.status, sheet::PREPARING_NOTE);
    assert!(model.enabled, "merging continues the preview's work");
    assert_eq!(model.merge(), Some(MergeAction::SheetMerge));
    assert_eq!(model.folder, "Konstanz");
    assert_eq!(model.folder_note, Some("catalog folder"));
    assert_eq!(
        model.output.as_deref(),
        Some("6048 \u{d7} 4024 \u{b7} 300 MB"),
        "the plan's estimate until the preview measures it"
    );
    let frames: Vec<_> = model
        .frames
        .iter()
        .map(|frame| {
            (
                frame.name.as_str(),
                frame.detail.as_str(),
                frame.value.as_str(),
                frame.reference,
            )
        })
        .collect();
    assert_eq!(
        frames,
        [
            (
                "DSC_4088.NEF",
                "\u{2212}2 EV \u{b7} 1/1000 s",
                "Auto",
                false
            ),
            ("DSC_4089.NEF", "0 EV \u{b7} 1/250 s", "Auto", true),
            ("DSC_4090.NEF", "+2 EV \u{b7} 1/60 s", "Auto", false),
        ]
    );

    // Ready: the picture, the status line, the output.
    sheet.adopt(revision(0));
    assert!(sheet.job.is_none());
    let model = sheet::model(&sheet, None);
    let Picture::Ready {
        path,
        width,
        height,
        regions,
        stitch,
        updating,
    } = &model.picture
    else {
        panic!("a picture: {:?}", model.picture);
    };
    assert!(stitch.is_none(), "HDR draws no stitch");
    assert_eq!(path, &PathBuf::from("/cache/merge-preview-0.jpg"));
    assert_eq!((*width, *height), (1024, 681));
    assert!(regions.is_empty(), "the overlay is off until shown");
    assert!(updating.is_none());
    assert_eq!(
        model.status,
        "Preview from 1,024-pixel frames \u{b7} aligned, 0.04 px"
    );
    assert_eq!(model.note, Some(sheet::DISPLAY_NOTE));
    assert_eq!(
        model.output.as_deref(),
        Some("6048 \u{d7} 4024 \u{b7} Highlights extended \u{b7} 279 MB")
    );

    // The overlay, drawn from the revision's data: tinted by exposure, labelled, fallback hatched.
    sheet.overlay = true;
    let model = sheet::model(&sheet, None);
    let Picture::Ready { regions, .. } = &model.picture else {
        panic!("a picture");
    };
    let drawn: Vec<_> = regions
        .iter()
        .map(|region| (region.tint, region.label.as_str(), region.fallback))
        .collect();
    assert_eq!(
        drawn,
        [
            (Tint::Reference, "From 0 EV", false),
            (Tint::Below, "From \u{2212}2 EV \u{b7} fallback", true),
        ]
    );
    assert_eq!(regions[0].bounds, [20.0, 300.0, 160.0, 50.0]);

    // A setting changed: the old picture stays while the update draws, with its job's words.
    sheet.request.options.alignment = Toggle::Off;
    sheet.following("job-2".into(), None);
    sheet.observe(&json!({"status": "running", "progress": {"message": "Aligning"}}));
    let Picture::Ready { updating, .. } = sheet::model(&sheet, None).picture else {
        panic!("the old picture");
    };
    assert_eq!(updating.as_deref(), Some("Aligning"));

    // A refused revision: its message in place of the picture and the status; no merges.
    let mut refused = revision(1);
    refused.path = None;
    refused.refusal = Some(json!({
        "reason": "same-exposure",
        "message": "Exposures must differ: DSC_4088.NEF and DSC_4089.NEF are both 0 EV",
    }));
    sheet.adopt(refused);
    let model = sheet::model(&sheet, None);
    let message = "Exposures must differ: DSC_4088.NEF and DSC_4089.NEF are both 0 EV";
    assert_eq!(model.picture, Picture::Refused(message.into()));
    assert!(model.status_refused && model.status == message);
    assert!(!model.enabled && model.merge().is_none());

    // Back to a good revision, but another merge of this desktop's runs.
    sheet.adopt(revision(2));
    let model = sheet::model(&sheet, Some(BUSY));
    assert!(!model.enabled);
    assert_eq!(model.reason.as_deref(), Some(BUSY));
}

/// A frame's override: stepped by thirds from its resolved offset, said in thirds, the reference
/// chosen by path; a plan that refuses opens no preview and says why in place of the picture.
#[test]
fn merge_preview_sheet_frames_step_by_thirds_and_a_plan_refusal_replaces_the_picture() {
    let mut sheet = open_sheet();
    sheet.planned(plan());
    let first = PathBuf::from("/cards/DSC_4088.NEF");
    sheet
        .request
        .options
        .set_override(first.clone(), Some(stepped(-2.0, 1)));
    sheet.request.options.reference = Some(PathBuf::from("/cards/DSC_4090.NEF"));
    let model = sheet::model(&sheet, None);
    assert_eq!(model.frames[0].value, "\u{2212}1\u{2154}");
    assert!(model.frames[0].overridden);
    assert!((model.frames[0].offset.unwrap() + 5.0 / 3.0).abs() < 1e-9);
    assert_eq!(
        model
            .frames
            .iter()
            .map(|frame| frame.reference)
            .collect::<Vec<_>>(),
        [false, false, true]
    );

    let mut refused = plan();
    refused.suitable = false;
    refused.refusal = Some(json!({"reason": "same-exposure", "message": "Exposures must differ"}));
    let mut sheet = open_sheet();
    sheet.planned(refused);
    let model = sheet::model(&sheet, None);
    assert_eq!(
        model.picture,
        Picture::Refused("Exposures must differ".into())
    );
    assert!(!model.enabled);
}

#[test]
fn merge_preview_words_and_geometry() {
    assert_eq!(
        files_words(&[
            "DSC_4088.NEF".into(),
            "DSC_4089.NEF".into(),
            "DSC_4090.NEF".into()
        ])
        .as_deref(),
        Some("DSC_4088\u{2013}4090.NEF")
    );
    assert_eq!(
        files_words(&["IMG_0999.CR3".into(), "IMG_1001.CR3".into()]).as_deref(),
        Some("IMG_0999\u{2013}1001.CR3")
    );
    assert_eq!(
        files_words(&["a.NEF".into(), "b.CR3".into()]).as_deref(),
        Some("a.NEF \u{2013} b.CR3")
    );
    assert_eq!(files_words(&["one.NEF".into()]).as_deref(), Some("one.NEF"));
    assert_eq!(files_words(&[]), None);
    assert_eq!(thirds_words(0.0), "0");
    assert_eq!(thirds_words(2.0 / 3.0), "+\u{2154}");
    assert_eq!(thirds_words(-4.0 / 3.0), "\u{2212}1\u{2153}");
    assert_eq!(thirds_words(2.0), "+2");
    assert!((stepped(0.1, 1) - EV_STEP).abs() < 1e-9, "onto the grid");
    assert!((stepped(-2.0, -1) + 7.0 / 3.0).abs() < 1e-9);
    assert_eq!(size_words(279 * 1024 * 1024), "279 MB");
    assert_eq!(size_words(3 * 1024 * 1024 * 1024 / 2), "1.5 GB");
    assert_eq!(shutter_words(1.0 / 1000.0), "1/1000 s");
    assert_eq!(shutter_words(2.0), "2 s");
    assert_eq!(
        crate::state::select::thousands(2048),
        "2,048",
        "the status line's proxy side"
    );
    let (origin, scale) = fit((1000.0, 500.0), (500.0, 500.0));
    assert_eq!((origin, scale), ([0.0, 125.0], 0.5));
    // A 10 × 10 square hatched every 5 along x + y: lines through it at 2.5, 7.5, … 17.5.
    let square = [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]];
    let lines = hatch(&square, 5.0);
    assert_eq!(lines.len(), 4);
    for (a, b) in lines {
        assert!(((a[0] + a[1]) - (b[0] + b[1])).abs() < 1e-4, "one diagonal");
        for point in [a, b] {
            assert!((-1e-4..=10.0001).contains(&point[0]) && (-1e-4..=10.0001).contains(&point[1]));
        }
    }
    assert!(hatch(&square[..2], 5.0).is_empty());
}

/// A panorama request of seven frames DSC_4101–4107.NEF.
fn panorama_request() -> MergeRequest {
    MergeRequest {
        kind: MergeKind::Panorama,
        targets: Targets::Files {
            file_ids: (1..=7).map(FileId).collect(),
        },
        options: MergeOptions::default(),
        source: None,
        anchor: 0,
        frames: 7,
    }
}

/// A plan of the seven frames, as `merge.plan` answers a panorama: no exposures, the panorama's
/// parameters and its estimates.
fn panorama_plan() -> MergePlan {
    let mut plan = plan();
    plan.kind = MergeKind::Panorama;
    plan.plan_id = "plan-pano".into();
    plan.inputs = (0..7)
        .map(|at| {
            let mut input = plan.inputs[0].clone();
            input.ordinal = at;
            input.path = PathBuf::from(format!("/cards/DSC_{}.NEF", 4101 + at));
            input.file_name = format!("DSC_{}.NEF", 4101 + at);
            input.file_id = Some(FileId(i64::from(at) + 1));
            input
        })
        .collect();
    plan.exposures = None;
    plan.reference = Some(3);
    plan.parameters = MergeParameters::Panorama(Default::default());
    plan.estimates.width = Some(38_400);
    plan.estimates.height = Some(8_900);
    plan.estimates.source_bytes = Some(4_400_000_000);
    plan
}

/// A ready panorama revision: the stack's order puts the plan's last frame first, the reference
/// is the stack's fourth, the crop loses 9% and every frame has its outline.
fn panorama_revision(revision: u32) -> MergePreview {
    let order: Vec<u32> = vec![6, 0, 1, 2, 3, 4, 5];
    MergePreview {
        preview_id: preview_id(),
        revision,
        kind: MergeKind::Panorama,
        options: WireOptions::Panorama(Default::default()),
        path: Some(PathBuf::from(format!("/cache/pano-{revision}.jpg"))),
        width: 1024,
        height: 237,
        diagnostics: Some(MergePreviewDiagnostics::Panorama(
            luxforge_core::catalog_types::PanoramaPreviewDiagnostics {
                proxy_side: 1024,
                projection: PanoramaProjection::Cylindrical,
                field_of_view_deg: [164.2, 40.6],
                crop_loss: 0.091,
                reference: 3,
                max_residual_px: 0.3,
                frames: order
                    .iter()
                    .map(|at| PathBuf::from(format!("/cards/DSC_{}.NEF", 4101 + at)))
                    .collect(),
            },
        )),
        overlays: Some(MergePreviewOverlays::Panorama(
            luxforge_core::catalog_types::PanoramaPreviewOverlays {
                covered: vec![vec![
                    [0.0, 0.0],
                    [1024.0, 0.0],
                    [1024.0, 237.0],
                    [0.0, 237.0],
                ]],
                crop: [20.0, 12.0, 984.0, 210.0],
                frames: (0..7)
                    .map(|frame| luxforge_core::catalog_types::PanoramaFrameOutline {
                        frame,
                        outline: vec![
                            [f64::from(frame) * 140.0, 0.0],
                            [f64::from(frame) * 140.0 + 180.0, 0.0],
                            [f64::from(frame) * 140.0 + 180.0, 237.0],
                        ],
                    })
                    .collect(),
            },
        )),
        output: Some(MergeOutput {
            width: 38_400,
            height: 8_900,
            scale: 1.0,
            source_bytes: 4_400_000_000,
        }),
        refusal: None,
    }
}

fn open_panorama() -> PreviewSheet {
    PreviewSheet::new(
        panorama_request(),
        SheetContext {
            names: vec!["DSC_4101.NEF".into(), "DSC_4107.NEF".into()],
            camera: Some("Nikon Z 8".into()),
            capture: None,
        },
        Vec::new(),
    )
}

/// A panorama's requests carry only what was chosen, under `panorama`: the reference by path, the
/// projection unless Auto and Boundary Warp unless 0; never HDR's fields.
#[test]
fn panorama_merge_requests_send_only_what_was_chosen() {
    let mut request = panorama_request();
    assert_eq!(
        plan_params(request.kind, &request.targets, &request.options),
        json!({"kind": "panorama", "targets": {"kind": "files", "file_ids": [1, 2, 3, 4, 5, 6, 7]}})
    );
    let id = preview_id();
    assert_eq!(
        preview_update_params(MergeKind::Panorama, &id, &request.options),
        json!({"preview_id": "merge-preview-0123456789abcdef0123456789abcdef", "panorama": {}})
    );
    request.options.alignment = Toggle::Off;
    request.options.projection = PanoramaProjection::Spherical;
    request.options.boundary_warp = 40;
    request.options.reference = Some("/cards/DSC_4104.NEF".into());
    assert_eq!(
        preview_update_params(MergeKind::Panorama, &id, &request.options),
        json!({
            "preview_id": "merge-preview-0123456789abcdef0123456789abcdef",
            "panorama": {"reference": "/cards/DSC_4104.NEF", "projection": "spherical", "boundary_warp": 40},
        })
    );
    let create = create_params(&request, CreateFrom::Preview(&id), None, &mutation());
    assert_eq!(create["kind"], "panorama");
    assert_eq!(create["preview_id"], json!(id));
    assert!(create.get("hdr").is_none() && create.get("plan_id").is_none());
    assert_eq!(create["panorama"]["projection"], "spherical");

    // ⌥-click's settings: the remembered projection and Boundary Warp, HDR's apart.
    let remembered = PanoramaMergePreference {
        projection: PanoramaProjection::Cylindrical,
        boundary_warp: 25,
    };
    let last = MergeOptions::last(None, Some(remembered));
    assert_eq!(
        Value::Object(last.wire(MergeKind::Panorama)),
        json!({"projection": "cylindrical", "boundary_warp": 25})
    );
    assert!(last.wire(MergeKind::Hdr).is_empty());
    assert_eq!(last.remembered_panorama(), remembered);
    assert_eq!(
        MergeOptions::last(None, None).remembered_panorama(),
        PanoramaMergePreference::default()
    );
}

/// A panorama sheet's states: its title and frames while reading, the ready picture with its
/// status line, numbered frames in the stack's order with the reference named, Auto's choice,
/// the output, and the overlays drawn only while shown.
#[test]
fn panorama_merge_sheet_draws_the_stitch_its_frames_and_overlays() {
    let mut sheet = open_panorama();
    let model = sheet::model(&sheet, None);
    assert_eq!(model.title, "Merge panorama \u{b7} 7 photos");
    assert_eq!(model.kind, MergeKind::Panorama);
    assert!(model.reading);
    let settings = model.panorama.expect("a panorama's settings");
    assert_eq!(settings.projection, PanoramaProjection::Auto);
    assert_eq!(settings.resolved, None);
    assert!(settings.outlines && settings.crop);
    assert_eq!(settings.frames_legend, "Frames 1\u{2013}7");

    // Planned: the frames in the plan's order, unnumbered, the plan's reference named.
    sheet.planned(panorama_plan());
    let model = sheet::model(&sheet, None);
    let names: Vec<_> = model
        .frames
        .iter()
        .map(|frame| frame.name.as_str())
        .collect();
    assert_eq!(names[0], "DSC_4101.NEF");
    assert!(model.frames.iter().all(|frame| frame.number.is_none()));
    assert_eq!(
        model.panorama.unwrap().reference.as_deref(),
        Some("Reference: DSC_4104.NEF")
    );
    assert_eq!(
        model.output.as_deref(),
        Some("38,400 \u{d7} 8,900 \u{b7} 342 MP \u{b7} 4.1 GB")
    );

    // Ready: the stack's order and numbers, the reference by its place, Auto's choice.
    sheet.adopt(panorama_revision(0));
    let model = sheet::model(&sheet, None);
    assert_eq!(
        model.status,
        "Preview from 1,024-pixel frames \u{b7} Cylindrical, 164\u{b0} \u{d7} 41\u{b0} \u{b7} Crop loses 9% at the edges"
    );
    let numbered: Vec<_> = model
        .frames
        .iter()
        .map(|frame| (frame.number.unwrap(), frame.name.as_str()))
        .collect();
    assert_eq!(numbered[0], (1, "DSC_4107.NEF"));
    assert_eq!(numbered[1], (2, "DSC_4101.NEF"));
    let reference: Vec<_> = model
        .frames
        .iter()
        .filter(|frame| frame.reference)
        .collect();
    assert_eq!(reference.len(), 1);
    assert_eq!(reference[0].name, "DSC_4103.NEF");
    let settings = model.panorama.clone().unwrap();
    assert_eq!(
        settings.reference.as_deref(),
        Some("Reference: 4 \u{b7} DSC_4103.NEF")
    );
    assert_eq!(
        settings.resolved.as_deref(),
        Some("Auto chose Cylindrical for a 164\u{b0} \u{d7} 41\u{b0} field.")
    );
    let Picture::Ready {
        stitch: Some(stitch),
        regions,
        ..
    } = &model.picture
    else {
        panic!("a stitched picture: {:?}", model.picture);
    };
    assert!(regions.is_empty(), "no deghost regions on a panorama");
    assert_eq!(stitch.crop, Some([20.0, 12.0, 984.0, 210.0]));
    assert_eq!(stitch.covered.len(), 1);
    let numbers: Vec<_> = stitch.frames.iter().map(|(number, _)| *number).collect();
    assert_eq!(numbers, [1, 2, 3, 4, 5, 6, 7]);
    assert_eq!(
        model.output.as_deref(),
        Some("38,400 \u{d7} 8,900 \u{b7} 342 MP \u{b7} 4.1 GB")
    );
    assert!(model.enabled);

    // A projection chosen: Auto's caption goes; the overlays hide when switched off.
    sheet.request.options.projection = PanoramaProjection::Perspective;
    sheet.outlines = false;
    let model = sheet::model(&sheet, None);
    assert_eq!(model.panorama.unwrap().resolved, None);
    let Picture::Ready {
        stitch: Some(stitch),
        ..
    } = &model.picture
    else {
        panic!("a stitched picture");
    };
    assert!(stitch.frames.is_empty() && stitch.crop.is_some());
    sheet.crop = false;
    let model = sheet::model(&sheet, None);
    let Picture::Ready {
        stitch: Some(stitch),
        ..
    } = &model.picture
    else {
        panic!("a stitched picture");
    };
    assert!(stitch.crop.is_none() && stitch.covered.is_empty());

    // A person's reference names it by its place in the stack.
    sheet.request.options.reference = Some("/cards/DSC_4107.NEF".into());
    let model = sheet::model(&sheet, None);
    assert_eq!(
        model.panorama.unwrap().reference.as_deref(),
        Some("Reference: 1 \u{b7} DSC_4107.NEF")
    );

    // A refusal replaces the picture and disables Merge.
    sheet.failed("Too little overlap between DSC_4104.NEF and DSC_4105.NEF".into());
    let model = sheet::model(&sheet, None);
    assert!(matches!(model.picture, Picture::Refused(_)));
    assert!(model.status_refused && !model.enabled);
}

/// The crop's words round to whole percent, with a sliver said as under 1%.
#[test]
fn panorama_merge_crop_words() {
    assert_eq!(sheet::crop_words(0.0), "Crop loses 0% at the edges");
    assert_eq!(sheet::crop_words(0.003), "Crop loses under 1% at the edges");
    assert_eq!(sheet::crop_words(0.091), "Crop loses 9% at the edges");
    assert_eq!(sheet::crop_words(2.0), "Crop loses 100% at the edges");
}

/// A panorama stack's words: "Panorama · 7 photos" and "Pano · 7" on its result, "Frame 2 of 7"
/// under each original, its own icon, and the Info band's projection and field of view as
/// `merge.inspect` answers them, "Reading the merge…" until it does.
#[test]
fn panorama_merge_stack_words_footers_and_band() {
    assert_eq!(
        stack_words(MergeKind::Panorama, 7),
        ("Panorama \u{b7} 7 photos", "Pano \u{b7} 7")
    );
    let merge_id: MergeId =
        serde_json::from_value(json!("merge-0123456789abcdef0123456789abcdef")).unwrap();
    let mut stack = RowStack {
        merge_id: merge_id.clone(),
        kind: MergeKind::Panorama,
        role: StackRole::Original,
        size: 7,
        collapsed: 0,
        expansion: None,
        ordinal: Some(1),
        offset_ev: None,
        outside_filter: false,
    };
    let cell = stack_cell(Some(&stack), None);
    assert_eq!(cell.exposure, Some("Frame 2 of 7"));
    assert_eq!(cell.icon, Some(Icon::Panorama));
    assert_eq!(stack_band(&stack, None).title, "Frame 2 of 7");
    assert_eq!(panorama_leader(Some(&stack)), None);

    stack.role = StackRole::Leader;
    stack.ordinal = None;
    stack.expansion = Some(StackExpansion::Collapsed);
    let cell = stack_cell(Some(&stack), None);
    assert_eq!(
        cell.badge.map(|(long, short, _)| (long, short)),
        Some(("Panorama \u{b7} 7 photos", "Pano \u{b7} 7"))
    );
    assert_eq!(panorama_leader(Some(&stack)), Some(&merge_id));
    let band = stack_band(&stack, None);
    assert_eq!(band.title, "Panorama \u{b7} 7 photos");
    assert_eq!(
        band_detail(&band, None).as_deref(),
        Some("Reading the merge\u{2026}")
    );
    let facts = PanoramaFacts {
        projection: Some(PanoramaProjection::Cylindrical),
        field_of_view_deg: Some([164.2, 40.6]),
    };
    assert_eq!(
        band_detail(&band, Some(&(merge_id.clone(), Some(Ok(facts))))).as_deref(),
        Some("Cylindrical \u{b7} 164\u{b0} \u{d7} 41\u{b0}")
    );
    assert_eq!(
        band_detail(&band, Some(&(merge_id.clone(), Some(Err("gone".into()))))).as_deref(),
        Some("gone")
    );
    assert_eq!(
        inspect_params(&merge_id),
        json!({"merge_id": "merge-0123456789abcdef0123456789abcdef"})
    );
    // An HDR stack's band reads nothing more.
    stack.kind = MergeKind::Hdr;
    assert_eq!(band_detail(&stack_band(&stack, None), None), None);
}
