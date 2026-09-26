//! A mask is stored in content coordinates, so it must land on the same content pixels after a
//! crop, a straighten, a quarter-turn and a reflection. This proves it by rendered comparison,
//! not by inspecting the payload: each tail is rendered twice, once with the masked layer and once
//! without, and the pixels that differ are mapped back through `render.transform`'s own inverse and
//! compared against the pixels that differ with no tail at all.
//!
//! The second test is the other half of the same claim over time: a reopened catalog gives back the
//! masks, their components, the layers bound to them and the history, with the identities it wrote.
use super::*;
use luxforge_core::{
    AssetId, EditorService, EntryId, MaskId, Mutation, Raster, StageTransform, Transform,
    mask::commands,
};
use serde_json::{Value, json};

/// The gradient this test draws: coverage 0 above `Y0` of the content stage, 1 below `Y1`.
const Y0: f64 = 0.30;
const Y1: f64 = 0.70;
/// Enough exposure that every unclipped covered pixel moves by more than one code.
const EXPOSURE: f64 = 2.0;

/// One geometry tail, as the commands that build it.
struct Tail {
    name: &'static str,
    /// Exact transforms, applied before the crop [`crop_for`] gives this tail.
    transforms: &'static [Transform],
}

const TAILS: [Tail; 5] = [
    Tail {
        name: "no tail",
        transforms: &[],
    },
    Tail {
        name: "crop",
        transforms: &[],
    },
    Tail {
        name: "straighten",
        transforms: &[],
    },
    Tail {
        name: "quarter turn",
        transforms: &[Transform::RotateRight],
    },
    Tail {
        name: "reflection",
        transforms: &[Transform::MirrorHorizontal],
    },
];

/// The crop each named tail applies, kept beside [`TAILS`] because a `const` cannot build a `Value`.
fn crop_for(name: &str) -> Option<Value> {
    match name {
        "crop" => Some(json!({"x": 0.10, "y": 0.20, "width": 0.70, "height": 0.65})),
        // A straighten resamples, so the mask's edge lands between output pixels: the comparison
        // below only checks pixels whose content neighbourhood is unambiguous.
        "straighten" => {
            Some(json!({"angle": 7.0, "x": 0.15, "y": 0.15, "width": 0.60, "height": 0.60}))
        }
        _ => None,
    }
}

struct Fixture {
    service: EditorService,
    asset: AssetId,
    revision: u64,
}

impl Fixture {
    fn open(name: &str) -> Self {
        let dir = temp(name);
        let source = dir.join("orientation-1.jpg");
        std::fs::copy(luxforge_testkit::fixtures::jpeg(), &source).unwrap();
        let mut service = EditorService::open(&dir.join("catalog.sqlite")).unwrap();
        let asset = service.import(&source).unwrap().asset.id;
        Self {
            service,
            asset,
            revision: 0,
        }
    }

    fn mutation(&mut self, request: &str) -> Mutation {
        let mutation = Mutation {
            expected_revision: self.service.state(&self.asset).unwrap().revision,
            request_id: format!("{request}-{}", self.revision),
            actor: "geometry".to_owned(),
        };
        self.revision += 1;
        mutation
    }

    fn mask(&mut self) -> MaskId {
        let command = commands::find("mask.create-linear").expect("a declared command");
        let mutation = self.mutation("create");
        self.service
            .run_action(
                &self.asset,
                mutation,
                command.method,
                json!({"x0": 0.5, "y0": Y0, "x1": 0.5, "y1": Y1}),
            )
            .expect("the gradient commits")
            .mask
            .expect("a created mask")
    }

    fn edit(&mut self, action: &str, parameters: Value) {
        let mutation = self.mutation(action);
        self.service
            .apply_action(&self.asset, mutation, action, parameters)
            .unwrap_or_else(|error| panic!("{action} failed: {error}"));
    }

    fn apply_tail(&mut self, tail: &Tail) {
        for transform in tail.transforms {
            let mutation = self.mutation("transform");
            self.service
                .apply_transform(&self.asset, mutation, *transform)
                .expect("an exact transform");
        }
        if let Some(crop) = crop_for(tail.name) {
            self.edit("crop", crop);
        }
    }

    fn entry(&self) -> EntryId {
        self.service.state(&self.asset).unwrap().current_entry.id
    }

    fn render(&self) -> Raster {
        self.service.render_current(&self.asset).expect("a render")
    }

    fn transform(&self) -> StageTransform {
        self.service
            .transform_entry(&self.asset, &self.entry())
            .expect("the geometry tail as one affine")
    }
}

/// Every output pixel whose byte differs between the two renders.
fn changed(masked: &Raster, plain: &Raster) -> Vec<bool> {
    assert_eq!(
        (masked.width, masked.height),
        (plain.width, plain.height),
        "a mask changes no dimension"
    );
    masked
        .rgba
        .chunks_exact(4)
        .zip(plain.rgba.chunks_exact(4))
        .map(|(a, b)| a != b)
        .collect()
}

/// A content coordinate mapped to its pixel index, or `None` outside the content stage.
fn content_pixel(
    inverse: &[f64; 6],
    width: u32,
    height: u32,
    x: u32,
    y: u32,
) -> Option<(u32, u32)> {
    let (ox, oy) = (f64::from(x) + 0.5, f64::from(y) + 0.5);
    let cx = inverse[0] * ox + inverse[1] * oy + inverse[2];
    let cy = inverse[3] * ox + inverse[4] * oy + inverse[5];
    if !(0.0..f64::from(width)).contains(&cx) || !(0.0..f64::from(height)).contains(&cy) {
        return None;
    }
    Some((cx as u32, cy as u32))
}

/// Whether the content neighbourhood of one pixel agrees about being changed, out to `reach`.
///
/// An exact tail maps pixel centres onto pixel centres, so one pixel of reach is all it needs. A
/// straighten resamples, and its kernel reads further than the pixel a coordinate lands in, so an
/// output pixel near the gradient's own edge mixes covered and uncovered input and cannot be
/// classified either way; only an unambiguous neighbourhood is compared.
fn settled(map: &[bool], width: u32, height: u32, x: u32, y: u32, reach: i64) -> Option<bool> {
    let here = map[(y * width + x) as usize];
    for dy in -reach..=reach {
        for dx in -reach..=reach {
            let nx = x as i64 + dx;
            let ny = y as i64 + dy;
            if nx < 0 || ny < 0 || nx >= i64::from(width) || ny >= i64::from(height) {
                continue;
            }
            if map[(ny as u32 * width + nx as u32) as usize] != here {
                return None;
            }
        }
    }
    Some(here)
}

#[test]
fn a_mask_lands_on_the_same_content_pixels_through_every_delivered_tail() {
    // The footprint with no tail at all: which content pixels the masked layer changes.
    let mut base = Fixture::open("content");
    let plain_content = base.render();
    let mask = base.mask();
    base.edit(
        "set-basic",
        json!({"mask": mask.as_str(), "exposure": EXPOSURE}),
    );
    let masked_content = base.render();
    let footprint = changed(&masked_content, &plain_content);
    let (width, height) = (masked_content.width, masked_content.height);
    assert!(
        footprint.iter().any(|changed| *changed) && footprint.iter().any(|changed| !*changed),
        "the gradient must change part of the content stage and leave part of it alone"
    );
    // The top of the content stage is uncovered and the bottom is fully covered, which is what
    // makes a footprint that moved rather than one that is everywhere or nowhere.
    assert!(
        !footprint[(width / 4) as usize],
        "the top-left content pixel sits at coverage 0 and must be untouched"
    );

    for tail in &TAILS {
        // The same tail twice: once with the masked layer, once without it. The masked layer is a
        // colour-stage edit, so the host places it before the tail whichever order they arrive in.
        let mut plain = Fixture::open(&format!("plain-{}", tail.name.replace(' ', "-")));
        plain.apply_tail(tail);
        let plain_render = plain.render();

        let mut masked = Fixture::open(&format!("masked-{}", tail.name.replace(' ', "-")));
        masked.apply_tail(tail);
        let id = masked.mask();
        masked.edit(
            "set-basic",
            json!({"mask": id.as_str(), "exposure": EXPOSURE}),
        );
        let masked_render = masked.render();

        let out = changed(&masked_render, &plain_render);
        let transform = masked.transform();
        // The straighten is the one delivered tail that resamples.
        let reach = if tail.name == "straighten" { 2 } else { 1 };
        assert_eq!(
            (transform.content.width, transform.content.height),
            (width, height),
            "{}: the content stage is the same stage the mask is stored against",
            tail.name
        );

        let mut compared = 0u32;
        let mut agreed = 0u32;
        for y in 0..masked_render.height {
            for x in 0..masked_render.width {
                let Some((cx, cy)) = content_pixel(&transform.inverse, width, height, x, y) else {
                    continue;
                };
                let Some(expected) = settled(&footprint, width, height, cx, cy, reach) else {
                    continue;
                };
                compared += 1;
                if out[(y * masked_render.width + x) as usize] == expected {
                    agreed += 1;
                }
            }
        }
        assert!(
            compared > 1_000,
            "{}: only {compared} output pixels could be compared",
            tail.name
        );
        assert_eq!(
            agreed,
            compared,
            "{}: {} of {compared} output pixels disagree with the content footprint the same mask \
             produces with no tail",
            tail.name,
            compared - agreed
        );
    }
}

#[test]
fn a_reopened_catalog_gives_back_the_masks_their_components_and_the_layers_bound_to_them() {
    let dir = temp("reopen");
    let catalog = dir.join("catalog.sqlite");
    let source = dir.join("orientation-1.jpg");
    std::fs::copy(luxforge_testkit::fixtures::jpeg(), &source).unwrap();

    let (asset, mask, component, layer, entries, label) = {
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&source).unwrap().asset.id;
        let mutation = |request: &str, expected: u64| Mutation {
            expected_revision: expected,
            request_id: request.to_owned(),
            actor: "reopen".to_owned(),
        };
        let created = service
            .run_action(
                &asset,
                mutation("create", 0),
                "mask.create-linear",
                json!({"x0": 0.5, "y0": Y0, "x1": 0.5, "y1": Y1}),
            )
            .expect("the gradient commits");
        let mask = created.mask.clone().expect("a created mask");
        let component = created.component.clone().expect("a created component");
        service
            .apply_action(
                &asset,
                mutation("lift", 1),
                "set-basic",
                json!({"mask": mask.as_str(), "exposure": EXPOSURE}),
            )
            .expect("a masked edit commits");
        let state = service.state(&asset).unwrap();
        let layer = state
            .current_entry
            .snapshot
            .recipe
            .layers
            .iter()
            .find(|layer| layer.mask.as_ref() == Some(&mask))
            .expect("a layer bound to the mask")
            .id
            .clone();
        let entries: Vec<EntryId> = service
            .history(&asset, None, 10)
            .unwrap()
            .entries
            .iter()
            .map(|entry| entry.id.clone())
            .collect();
        let label = state.current_entry.label.clone();
        (asset, mask, component, layer, entries, label)
    };

    // A new process would open the catalog exactly this way.
    let service = EditorService::open(&catalog).unwrap();
    let state = service.state(&asset).unwrap();
    assert_eq!(
        state.revision, 2,
        "the reopened asset is at its own revision"
    );
    assert_eq!(
        state.current_entry.label, label,
        "the history label survives"
    );

    let recipe = &state.current_entry.snapshot.recipe;
    let reopened = recipe
        .masks
        .iter()
        .find(|reopened| reopened.id == mask)
        .expect("the mask comes back with the identity it was written with");
    assert_eq!(
        reopened.components.len(),
        1,
        "the mask comes back with its one component"
    );
    assert_eq!(
        reopened.components[0].id, component,
        "the component keeps its identity"
    );
    assert_eq!(
        recipe
            .layers
            .iter()
            .filter(|reopened| reopened.mask.as_ref() == Some(&mask))
            .map(|reopened| reopened.id.clone())
            .collect::<Vec<_>>(),
        vec![layer],
        "the layer bound to the mask comes back bound to it, with its own identity"
    );

    // History navigation: every entry is still addressable by the identity it was written with, and
    // the entry before the masked edit holds the mask with no layer bound to it.
    let reopened_entries: Vec<EntryId> = service
        .history(&asset, None, 10)
        .unwrap()
        .entries
        .iter()
        .map(|entry| entry.id.clone())
        .collect();
    assert_eq!(
        reopened_entries, entries,
        "the history comes back with the same entries in the same order"
    );
    let create_entry = service
        .entry(&asset, entries.iter().last().expect("the oldest entry"))
        .expect("the oldest entry is addressable");
    assert!(
        create_entry.snapshot.recipe.masks.is_empty(),
        "the Original entry holds no mask"
    );
    let gradient = service
        .entry(&asset, &entries[1])
        .expect("the mask-create entry is addressable");
    assert_eq!(
        gradient.snapshot.recipe.masks.len(),
        1,
        "the mask-create entry holds the mask"
    );
    assert!(
        gradient
            .snapshot
            .recipe
            .layers
            .iter()
            .all(|layer| layer.mask.is_none()),
        "no layer is bound to it yet in that entry"
    );
    // And it still renders, at each entry, which is what history navigation shows a person.
    for entry in &entries {
        service
            .render_entry(&asset, entry)
            .unwrap_or_else(|error| panic!("a reopened entry renders: {error}"));
    }

    drop(service);
    std::fs::remove_dir_all(&dir).ok();
}
