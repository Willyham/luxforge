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
    let defaults = MergeOptions::last(None);
    assert_eq!(defaults, MergeOptions::default());
    assert!(defaults.wire().is_empty());
    let remembered = HdrMergePreference {
        alignment: AlignmentMode::Off,
        deghost: DeghostMode::Auto,
    };
    let last = MergeOptions::last(Some(remembered));
    assert_eq!((last.alignment, last.deghost), (Toggle::Off, Toggle::Auto));
    assert_eq!(Value::Object(last.wire()), json!({"alignment": "off"}));
    assert_eq!(last.remembered(), remembered);
    assert_eq!(
        MergeOptions::default().remembered(),
        HdrMergePreference::default()
    );
}

/// The selection menu's HDR group: its heading, its one Merge HDR item with its hint and the
/// ellipsis that ⌥ drops; a plan's refusal disables it with its reason, and so does a merge
/// already running.
#[test]
fn merge_preview_menu_is_the_hdr_group_and_option_drops_the_ellipsis() {
    let mut merge = SelectMerge {
        menu: Some(MergeMenu {
            x: 0.0,
            y: 0.0,
            kind: MergeMenuKind::Frames {
                kind: MergeKind::Hdr,
                count: 3,
                refusal: None,
            },
        }),
        ..SelectMerge::default()
    };
    let menu = derive(&merge).menu.unwrap();
    assert_eq!(menu.heading, Some("HDR"));
    let labels: Vec<_> = menu
        .entries
        .iter()
        .map(|entry| entry.label.as_str())
        .collect();
    assert_eq!(labels, ["Merge HDR\u{2026}"]);
    assert_eq!(
        menu.entries[0].hint,
        Some("Merge the frames into one HDR photograph in the catalog")
    );
    assert_eq!(
        menu.entries[0].action,
        Some(MergeAction::Start(MergeKind::Hdr))
    );
    merge.option = true;
    let menu = derive(&merge).menu.unwrap();
    assert_eq!(menu.entries[0].label, "Merge HDR");

    let reason = "Exposures must differ: DSC_4102.NEF and DSC_4103.NEF are both 0 EV";
    merge.menu = Some(MergeMenu {
        x: 0.0,
        y: 0.0,
        kind: MergeMenuKind::Frames {
            kind: MergeKind::Hdr,
            count: 7,
            refusal: Some(reason.into()),
        },
    });
    for entry in derive(&merge).menu.unwrap().entries {
        assert!(entry.action.is_none());
        assert_eq!(entry.reason.as_deref(), Some(reason));
    }
    merge.menu = Some(MergeMenu {
        x: 0.0,
        y: 0.0,
        kind: MergeMenuKind::Frames {
            kind: MergeKind::Hdr,
            count: 3,
            refusal: None,
        },
    });
    merge.starting = Some(request());
    for entry in derive(&merge).menu.unwrap().entries {
        assert!(entry.action.is_none());
        assert_eq!(entry.reason.as_deref(), Some(BUSY));
    }
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
        updating,
    } = &model.picture
    else {
        panic!("a picture: {:?}", model.picture);
    };
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
