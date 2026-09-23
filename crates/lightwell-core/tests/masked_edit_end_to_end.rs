//! The phase A vertical slice, end to end: create a mask through `mask.create`, edit through it with
//! `edit.set-basic`, and prove the picture changed only where the mask covers it.
//!
//! This is the one thing neither half of the work could prove on its own. The command family was
//! written before a layer could carry a mask, so its tests never rendered; the masked colour
//! primitive was written before `mask.create` existed, so its tests planted a mask table by hand.
//! Both are honest, and together they still leave the question a person actually asks — *does
//! dragging a gradient and raising exposure change one part of the picture and not the other* —
//! unanswered. It is answered here, over the same service every client reaches.
use lightwell_core::{
    AssetId, BASIC_EFFECT, EditorService, MaskId, Mutation, Raster,
    mask::commands::{self, MaskTarget},
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(1);

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lightwell-masked-edit-{}-{}-{name}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-1.jpg")
}

/// The luminance of one pixel, for comparing a lifted region against an untouched one.
fn luma(raster: &Raster, x: u32, y: u32) -> f64 {
    let p = raster.pixel(x, y).expect("pixel inside the stage");
    0.2126 * f64::from(p[0]) + 0.7152 * f64::from(p[1]) + 0.0722 * f64::from(p[2])
}

struct Fixture {
    service: EditorService,
    asset: AssetId,
}

impl Fixture {
    fn open(name: &str) -> Self {
        let dir = temp(name);
        let source = dir.join("orientation-1.jpg");
        std::fs::copy(fixture(), &source).unwrap();
        let mut service = EditorService::open(&dir.join("catalog.sqlite")).unwrap();
        let asset = service.import(&source).unwrap().asset.id;
        Self { service, asset }
    }

    fn mutation(&self, request: &str) -> Mutation {
        Mutation {
            expected_revision: self.service.state(&self.asset).unwrap().revision,
            request_id: request.to_owned(),
            actor: "end-to-end".to_owned(),
        }
    }

    /// One `mask.*` command, resolved through the same table `schema.list` publishes.
    fn mask_command(
        &mut self,
        method: &str,
        target: MaskTarget,
        parameters: Value,
        request: &str,
    ) -> MaskId {
        let command =
            commands::find(method).unwrap_or_else(|| panic!("{method} is a declared command"));
        let mutation = self.mutation(request);
        let result = self
            .service
            .apply_mask_command(&self.asset, mutation, command, parameters, target)
            .unwrap_or_else(|e| panic!("{method} failed: {e}"));
        result.mask.expect("a command that names a mask reports it")
    }

    fn edit(&mut self, action: &str, parameters: Value, request: &str) {
        let mutation = self.mutation(request);
        self.service
            .apply_action(&self.asset, mutation, action, parameters)
            .unwrap_or_else(|e| panic!("{action} failed: {e}"));
    }
}

#[test]
fn a_gradient_mask_lifts_one_side_of_the_picture_and_leaves_the_other_byte_identical() {
    let mut f = Fixture::open("gradient");
    let unmasked = f.service.render_current(&f.asset).unwrap();
    let (width, height) = (unmasked.width, unmasked.height);

    // A vertical gradient: no coverage at the top row, full coverage at the bottom row.
    let mask = f.mask_command(
        "mask.create",
        MaskTarget::default(),
        json!({"kind": "linear", "x0": 0.5, "y0": 0.0, "x1": 0.5, "y1": 1.0}),
        "create",
    );

    f.edit(
        "set-basic",
        json!({"mask": mask.as_str(), "exposure": 2.0}),
        "lift",
    );

    let masked = f.service.render_current(&f.asset).unwrap();
    assert_eq!(
        (masked.width, masked.height),
        (width, height),
        "a mask changes no dimension"
    );

    // The top row sits at coverage 0 and must be byte-identical to the unmasked render.
    let row = width as usize * 4;
    assert_eq!(
        &masked.rgba[..row],
        &unmasked.rgba[..row],
        "the uncovered top row must be untouched, byte for byte"
    );

    // The bottom row sits at coverage 1 and must be lifted.
    let y = height - 1;
    assert!(
        luma(&masked, width / 2, y) > luma(&unmasked, width / 2, y) + 1.0,
        "the covered bottom row must be lifted by +2 EV"
    );

    // A sample equals the rendered byte on both sides of the gradient and inside its ramp.
    let entry = f.service.state(&f.asset).unwrap().current_entry.id;
    for (x, y) in [
        (width / 2, 0),
        (width / 2, height / 2),
        (width / 2, height - 1),
    ] {
        let sample = f.service.sample_entry(&f.asset, &entry, x, y).unwrap();
        let rendered = masked.pixel(x, y).expect("pixel inside the stage");
        assert_eq!(
            sample.rgba, rendered,
            "render.sample must equal the rendered byte at ({x}, {y})"
        );
    }
}

#[test]
fn a_global_and_a_masked_layer_of_one_effect_coexist_in_mask_order() {
    let mut f = Fixture::open("coexist");
    let mask = f.mask_command(
        "mask.create",
        MaskTarget::default(),
        json!({"kind": "linear", "x0": 0.0, "y0": 0.5, "x1": 1.0, "y1": 0.5}),
        "create",
    );

    // The masked edit first, then the global one: the global layer must still land before it.
    f.edit(
        "set-basic",
        json!({"mask": mask.as_str(), "exposure": 1.0}),
        "masked",
    );
    f.edit("set-basic", json!({"exposure": -0.5}), "global");

    let state = f.service.state(&f.asset).unwrap();
    let targets: Vec<_> = state
        .current_entry
        .snapshot
        .recipe
        .layers
        .iter()
        .filter(|l| l.effect_id == BASIC_EFFECT)
        .map(|l| l.mask.clone())
        .collect();
    assert_eq!(
        targets,
        vec![None, Some(mask)],
        "one effect holds one global layer and one masked layer, the global one first"
    );

    // And the stack still renders, which "ambiguous Basic layers" would have prevented.
    f.service
        .render_current(&f.asset)
        .expect("a stack holding a global and a masked layer of one effect renders");
}
