//! The `mask.*` command family from an independent JSON client, through the owner loop with the
//! shared client (`luxforge_testkit::client`): a gesture drafted over a component, a draft that
//! another client commits under, discovery of the family as a host descriptor, a mask deleted with
//! the layers bound to it, and a masked recipe whose original is missing or changed. What each command does to a stack and the label it commits are the
//! command family's own tests (`mask::commands`), and a retry is the dispatcher's one envelope check.

use super::*;
use luxforge_core::{
    ClientId, ComponentId, EditorService, MaskId,
    mask::commands::{self, MaskTarget},
};
use luxforge_testkit::client::{Owner, mutation as envelope};

/// Who this module's mutations name.
const ACTOR: &str = "mask-commands";

fn mutation(revision: u64, request: &str) -> Value {
    envelope(revision, request, ACTOR)
}

/// One imported photograph behind an owner, and the client that talks to it.
struct Session {
    owner: Owner,
    client: ClientId,
    asset: Value,
    dir: PathBuf,
}

impl Session {
    fn open(name: &str) -> Self {
        let dir = temp(name);
        let source = dir.join("orientation-1.jpg");
        std::fs::copy(fixtures::jpeg(), &source).expect("the fixture copies");
        let owner = Owner::start(
            &dir.join("catalog.sqlite"),
            luxforge_core::ModuleRegistry::builtin(),
            ACTOR,
        )
        .expect("an owner");
        let client = owner.client();
        let asset =
            owner.import(client, &source).expect("the fixture imports")["asset"]["id"].clone();
        Self {
            owner,
            client,
            asset,
            dir,
        }
    }

    /// One call that must succeed, from this session's client.
    fn send(&self, method: &str, params: Value) -> Value {
        self.owner
            .call(self.client, method, params)
            .unwrap_or_else(|error| panic!("{error}"))
    }

    /// One call that must be refused, answering its message.
    fn refused(&self, method: &str, params: Value) -> String {
        self.owner
            .refused(self.client, method, params)
            .unwrap_or_else(|error| panic!("{error}"))
            .1
    }

    fn revision(&self) -> u64 {
        self.owner
            .revision(self.client, &self.asset)
            .expect("asset.state answers")
    }

    fn list(&self) -> Value {
        self.send("mask.list", json!({"asset_id": self.asset}))
    }

    /// Every entry as `[action_id, label]`, oldest first.
    fn labels(&self) -> Vec<Value> {
        let listed = self.send(
            "history.list",
            json!({"asset_id": self.asset, "limit": 100}),
        );
        let mut labels: Vec<Value> = listed["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| json!([entry["action_id"], entry["label"]]))
            .collect();
        labels.reverse();
        labels
    }

    /// One command at the current revision, with the target's identities beside its values.
    fn run(&self, method: &str, target: &MaskTarget, parameters: Value, request: &str) -> Value {
        let mut params = target.request(parameters);
        params["asset_id"] = self.asset.clone();
        params["mutation"] = mutation(self.revision(), request);
        self.send(method, params)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The component of `mask` named `component`, resolved the way a client resolves one: from the
/// listing.
fn component_of(listing: &Value, mask: &str, component: &str) -> MaskTarget {
    let found = listing["masks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|candidate| candidate["name"] == json!(mask))
        .unwrap_or_else(|| panic!("no mask named {mask}"));
    let id = found["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|candidate| candidate["name"] == json!(component))
        .unwrap_or_else(|| panic!("no component named {component} in {mask}"))["id"]
        .as_str()
        .unwrap();
    MaskTarget {
        mask: Some(MaskId::parse(found["id"].as_str().unwrap()).unwrap()),
        component: Some(ComponentId::parse(id).unwrap()),
        ..MaskTarget::default()
    }
}

fn linear(x0: f64, y0: f64, x1: f64, y1: f64) -> Value {
    json!({"x0": x0, "y0": y0, "x1": x1, "y1": y1})
}

#[test]
fn a_gradient_drag_is_one_entry_and_a_drag_that_returns_to_its_start_is_none() {
    let client = Session::open("draft");
    let asset = client.asset.clone();
    client.run(
        "mask.create-linear",
        &MaskTarget::default(),
        linear(0.0, 0.0, 0.0, 1.0),
        "create",
    );
    let listing = client.list();
    let target = component_of(&listing, "Mask 1", "Linear 1");
    let begin = json!({
        "asset_id": asset,
        "action": "mask.set-linear",
        "mask": target.mask.as_ref().unwrap().as_str(),
        "component": target.component.as_ref().unwrap().as_str(),
    });
    // One drag: pointer-down, many moves, one release, one entry.
    let draft = client.send("draft.begin", begin.clone());
    let draft_id = draft["draft_id"].clone();
    for y in [0.9, 0.8, 0.7, 0.6] {
        client.send(
            "draft.set",
            json!({"draft_id": draft_id, "fields": {"y1": y}}),
        );
    }
    let before = client.labels().len();
    let revision = client.revision();
    let committed = client.send(
        "draft.commit",
        json!({"draft_id": draft_id, "mutation": mutation(revision, "drag")}),
    );
    assert_eq!(committed["outcome"], json!("applied"));
    assert_eq!(committed["label"], json!("Update Linear 1"));
    let after = client.labels();
    assert_eq!(
        after.len(),
        before + 1,
        "one entry per gesture, never one per pointer move"
    );
    assert_eq!(
        client.list()["masks"][0]["components"][0]["payload"],
        json!({"x0": 0.0, "y0": 0.0, "x1": 0.0, "y1": 0.6}),
        "the drafted geometry is what committed"
    );
    // A drag that wanders and returns to where it began commits nothing and ends the draft anyway.
    let draft = client.send("draft.begin", begin);
    let draft_id = draft["draft_id"].clone();
    for y in [0.4, 0.2, 0.6] {
        client.send(
            "draft.set",
            json!({"draft_id": draft_id, "fields": {"y1": y}}),
        );
    }
    let revision = client.revision();
    let nothing = client.send(
        "draft.commit",
        json!({"draft_id": draft_id, "mutation": mutation(revision, "return")}),
    );
    assert_eq!(nothing["outcome"], json!("no-op"));
    assert_eq!(
        nothing["revision"],
        json!(revision),
        "the stack did not move"
    );
    assert!(
        nothing.get("label").is_none(),
        "a no-op wrote no entry, so there is no label: {nothing}"
    );
    assert_eq!(client.labels(), after, "no entry and no event");
    assert!(
        client.send("session.state", json!({}))["draft"].is_null(),
        "the gesture is over either way"
    );
    // A draft of a maskable module action takes the mask — that is what makes a masked slider
    // preview what it will commit — but never a component, and an action of a module with no
    // maskable effect takes neither.
    let mask_id = target.mask.as_ref().unwrap().as_str().to_owned();
    let opened = client.send(
        "draft.begin",
        json!({"asset_id": asset, "action": "set-basic", "mask": mask_id}),
    );
    assert_eq!(opened["target"]["mask"], json!(mask_id));
    assert!(
        opened["target"].get("component").is_none(),
        "a module action's draft carries the mask alone: {opened}"
    );
    client.send("draft.cancel", json!({"draft_id": opened["draft_id"]}));
    assert_eq!(
        client.refused(
            "draft.begin",
            json!({"asset_id": asset, "action": "set-basic", "mask": mask_id,
                   "component": target.component.as_ref().unwrap().as_str()}),
        ),
        "unknown parameter component for action set-basic"
    );
    assert_eq!(
        client.refused(
            "draft.begin",
            json!({"asset_id": asset, "action": "crop", "mask": mask_id}),
        ),
        "action crop does not accept a mask target"
    );
    // A gesture names every object its command addresses when it begins: a patch without its
    // component, and a stroke deletion — whose stroke a draft cannot carry — are refused there.
    assert_eq!(
        client.refused(
            "draft.begin",
            json!({"asset_id": asset, "action": "mask.set-linear", "mask": mask_id}),
        ),
        "missing required parameter component for action mask.set-linear"
    );
    assert_eq!(
        client.refused(
            "draft.begin",
            json!({"asset_id": asset, "action": "mask.delete-stroke", "mask": mask_id,
                   "component": target.component.as_ref().unwrap().as_str()}),
        ),
        "missing required parameter stroke for action mask.delete-stroke"
    );
    assert_eq!(
        client.refused(
            "draft.begin",
            json!({"asset_id": asset, "action": "mask.create-linear", "mask": mask_id}),
        ),
        "unknown parameter mask for action mask.create-linear"
    );
}

#[test]
fn an_external_commit_conflicts_a_mask_draft_and_discard_or_reapply_resolves_it() {
    let mine = Session::open("conflict");
    let asset = mine.asset.clone();
    let other = mine.owner.client();
    mine.run(
        "mask.create-linear",
        &MaskTarget::default(),
        linear(0.0, 0.0, 0.0, 1.0),
        "create",
    );
    let listing = mine.list();
    let target = component_of(&listing, "Mask 1", "Linear 1");
    let begin = json!({
        "asset_id": asset,
        "action": "mask.set-linear",
        "mask": target.mask.as_ref().unwrap().as_str(),
        "component": target.component.as_ref().unwrap().as_str(),
    });
    let draft = mine.send("draft.begin", begin.clone());
    let draft_id = draft["draft_id"].clone();
    let base = draft["base_revision"].as_u64().unwrap();
    mine.send(
        "draft.set",
        json!({"draft_id": draft_id, "fields": {"y1": 0.3}}),
    );
    // Another client commits under the draft.
    mine.owner
        .call(
            other,
            "mask.set-amount",
            json!({"asset_id": asset, "mutation": mutation(base, "external"),
                   "mask": target.mask.as_ref().unwrap().as_str(), "amount": 40}),
        )
        .expect("another client may commit");
    let read = mine.send("draft.read", json!({"draft_id": draft_id}));
    assert_eq!(
        read["conflicted"],
        json!(true),
        "the asset moved under the draft"
    );
    let revision = mine.revision();
    assert_eq!(
        mine.refused(
            "draft.commit",
            json!({"draft_id": draft_id, "mutation": mutation(revision, "conflicted")}),
        ),
        "the asset changed under this draft; discard it or reapply it"
    );
    // Reapply rebases this client's own fields over what the other client committed.
    let reapplied = mine.send("draft.reapply", json!({"draft_id": draft_id}));
    assert_eq!(reapplied["conflicted"], json!(false));
    assert_eq!(reapplied["base_revision"], json!(revision));
    let committed = mine.send(
        "draft.commit",
        json!({"draft_id": draft_id, "mutation": mutation(revision, "reapplied")}),
    );
    assert_eq!(committed["label"], json!("Update Linear 1"));
    let listed = mine.list();
    assert_eq!(
        listed["masks"][0]["amount"],
        json!(40.0),
        "the other client's amount survived"
    );
    assert_eq!(
        listed["masks"][0]["components"][0]["payload"]["y1"],
        json!(0.3),
        "and so did this client's own drafted field"
    );
    // Discard leaves the stack exactly as it is.
    let before = mine.labels();
    let draft = mine.send("draft.begin", begin);
    let draft_id = draft["draft_id"].clone();
    mine.send(
        "draft.set",
        json!({"draft_id": draft_id, "fields": {"y1": 0.9}}),
    );
    assert_eq!(
        mine.send("draft.cancel", json!({"draft_id": draft_id}))["cancelled"],
        json!(true)
    );
    assert_eq!(mine.labels(), before, "a discarded draft wrote nothing");
    assert_eq!(
        mine.list()["masks"][0]["components"][0]["payload"]["y1"],
        json!(0.3)
    );
}

/// An independent client discovers the mask family exactly as it discovers a module's actions: from
/// `module.list`, where the host publishes one descriptor in the module shape, whose actions and
/// queries are the methods `schema.list` lists with the same parameters — the identities among them,
/// of the identity kind — and whose identities are refused by the one generic check, from either
/// side, with the same words.
#[test]
fn the_mask_family_is_discovered_as_a_host_descriptor_and_checked_like_any_action() {
    let client = Session::open("discovery");
    let listed = client.send("module.list", json!({}));
    let host = listed["host"].as_array().expect("the host's descriptors");
    assert_eq!(host.len(), 1);
    let masks = &host[0];
    assert_eq!(masks["id"], json!("luxforge.masks"));
    assert!(
        listed["modules"]
            .as_array()
            .unwrap()
            .iter()
            .all(|module| module["id"] != json!("luxforge.masks")),
        "the host is listed beside the modules, not as one"
    );
    let schema = client.send("schema.list", json!({}));
    let methods = &schema["methods"];
    let actions = masks["actions"].as_array().unwrap();
    assert_eq!(actions.len(), commands::all().len());
    for action in actions.iter().chain(masks["queries"].as_array().unwrap()) {
        let method = action["id"].as_str().unwrap();
        assert_eq!(
            methods[method]["parameters"], action["parameters"],
            "{method} is listed with the parameters its descriptor declares"
        );
    }
    assert_eq!(schema["host"], listed["host"]);
    // The identities are declared parameters of the identity kind, first and in a fixed order.
    let patch = actions
        .iter()
        .find(|action| action["id"] == json!("mask.set-linear"))
        .unwrap();
    assert_eq!(
        patch["parameters"][0],
        json!({"name": "mask", "kind": "identity", "of": "mask", "required": true,
               "default": null, "unit": null, "step": null, "precision": null,
               "notes": "the mask this command addresses"})
    );
    assert_eq!(patch["parameters"][1]["of"], json!("component"));
    // A maskable module action declares its host `mask` field as the same kind, as its target.
    assert_eq!(
        methods["edit.set-basic"]["target"]["kind"],
        json!("identity")
    );
    assert_eq!(methods["edit.set-basic"]["target"]["of"], json!("mask"));

    // One generic check refuses a malformed identity, from an independent client and from inside,
    // on a mask command and on a masked module action alike.
    let wrong = ComponentId::new();
    let revision = client.revision();
    let json_refusals = [
        client.owner.refused(
            client.client,
            "mask.set-amount",
            json!({"asset_id": client.asset, "mutation": mutation(revision, "a"),
                   "mask": wrong.as_str(), "amount": 20}),
        ),
        client.owner.refused(
            client.client,
            "edit.set-basic",
            json!({"asset_id": client.asset, "mutation": mutation(revision, "b"),
                   "mask": wrong.as_str(), "exposure": 0.5}),
        ),
    ]
    .map(|refused| {
        let (code, detail) = refused.unwrap_or_else(|error| panic!("{error}"));
        json!({"code": code, "detail": detail})
    });
    let source = client.dir.join("orientation-1.jpg");
    let mut service = EditorService::open(&client.dir.join("direct.sqlite")).unwrap();
    let asset = service.import(&source).unwrap().asset.id;
    let direct_refusals = ["mask.set-amount", "set-basic"].map(|action| {
        let parameters = match action {
            "mask.set-amount" => json!({"mask": wrong.as_str(), "amount": 20}),
            _ => json!({"mask": wrong.as_str(), "exposure": 0.5}),
        };
        let error = service
            .run_action(
                &asset,
                fixtures::mutation(1, action, ACTOR),
                action,
                parameters,
            )
            .expect_err("a component is not a mask");
        json!({"code": error.kind.code(), "detail": error.detail})
    });
    for refusals in [&json_refusals, &direct_refusals] {
        for refusal in refusals {
            assert_eq!(
                refusal,
                &json!({"code": "validation", "detail": "parameter mask must be a mask identity"})
            );
        }
    }
}

#[test]
fn deleting_a_mask_carrying_layers_of_two_effects_says_what_it_removed() {
    let client = Session::open("delete");
    // A mask, a global Basic layer, and a Basic and a Presence layer drawn through the mask.
    let mask = client.run(
        "mask.create-linear",
        &MaskTarget::default(),
        linear(0.0, 0.0, 0.0, 1.0),
        "create",
    )["mask"]
        .clone();
    for (method, request, fields) in [
        ("edit.set-basic", "global", json!({"exposure": 0.5})),
        (
            "edit.set-basic",
            "masked-basic",
            json!({"mask": mask, "exposure": 0.5}),
        ),
        (
            "edit.set-presence",
            "masked-presence",
            json!({"mask": mask, "clarity": 20.0}),
        ),
    ] {
        let mut params = fields;
        params["asset_id"] = client.asset.clone();
        params["mutation"] = mutation(client.revision(), request);
        client.send(method, params);
    }
    let layers = client
        .owner
        .recipe(client.client, &client.asset)
        .expect("the recipe")
        .layers;
    let unmasked = layers
        .iter()
        .find(|layer| layer.mask.is_none())
        .expect("the global layer")
        .id
        .clone();
    let masked: Vec<_> = layers
        .iter()
        .filter(|layer| layer.mask.is_some())
        .map(|layer| layer.id.clone())
        .collect();
    let mask = MaskTarget {
        mask: Some(MaskId::parse(mask.as_str().unwrap()).unwrap()),
        ..MaskTarget::default()
    };
    let listed = client.list();
    assert_eq!(
        listed["masks"][0]["layers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|layer| layer["title"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["Basic", "Presence"],
        "mask.list reports the layers bound to the mask"
    );
    let removed = client.run("mask.delete", &mask, Value::Null, "delete");
    assert_eq!(
        removed["label"],
        json!("Delete Mask 1 with Basic, Presence"),
        "the history label names exactly what went with the mask"
    );
    assert_eq!(
        removed["removed_layers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|layer| json!([layer["id"], layer["effect"], layer["title"]]))
            .collect::<Vec<_>>(),
        masked
            .iter()
            .zip([BASIC_EFFECT, luxforge_core::PRESENCE_EFFECT])
            .map(|(id, effect)| {
                let title = if effect == BASIC_EFFECT {
                    "Basic"
                } else {
                    "Presence"
                };
                json!([id.as_str(), effect, title])
            })
            .collect::<Vec<_>>(),
        "and the result names them by identity too"
    );
    assert!(
        client.list()["masks"].as_array().unwrap().is_empty(),
        "the mask is gone"
    );
    let described = client.send("recipe.describe", json!({"asset_id": client.asset}));
    assert_eq!(
        described["layers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|layer| layer["id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>(),
        [unmasked.as_str().to_owned()],
        "the global layer of the same effect stayed exactly where it was"
    );
    // The entry that removed them keeps the complete stack that preceded it, so undo restores both.
    let revision = client.revision();
    client.send(
        "history.undo",
        json!({"asset_id": client.asset, "mutation": mutation(revision, "undo")}),
    );
    assert_eq!(
        client.list()["masks"][0]["layers"]
            .as_array()
            .unwrap()
            .len(),
        2,
        "originals are sacred and so is history: the removed layers come back"
    );

    // And the other half of the relation, over the restored stack: a duplicate copies the layers
    // bound to the mask as well as its components, because a mask without its adjustments is not a
    // useful copy. The copies are legal because `single_layer` is per target and the two masks are
    // two targets, and the whole stack compiles on the way out — which is what `mask.list`
    // answering at all proves.
    client.run("mask.duplicate", &mask, Value::Null, "duplicate");
    let listed = client.list();
    let masks = listed["masks"].as_array().expect("two masks");
    assert_eq!(masks.len(), 2);
    for report in masks {
        assert_eq!(
            report["layers"]
                .as_array()
                .unwrap()
                .iter()
                .map(|layer| layer["title"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["Basic", "Presence"],
            "each mask holds its own copy of the adjustments: {report}"
        );
    }
    assert_ne!(
        masks[0]["layers"][0]["id"], masks[1]["layers"][0]["id"],
        "a copied layer takes a new identity"
    );
    // The durable processing order follows the ordering rule: the global layer first, then each
    // effect's masked layers in the order the mask list shows.
    let described = client.send("recipe.describe", json!({"asset_id": client.asset}));
    let order: Vec<Value> = described["layers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|layer| json!([layer["effect"], layer["mask"]]))
        .collect();
    assert_eq!(
        order,
        json!([
            [BASIC_EFFECT, Value::Null],
            [BASIC_EFFECT, masks[0]["id"]],
            [BASIC_EFFECT, masks[1]["id"]],
            [luxforge_core::PRESENCE_EFFECT, masks[0]["id"]],
            [luxforge_core::PRESENCE_EFFECT, masks[1]["id"]],
        ])
        .as_array()
        .unwrap()
        .clone(),
        "each copy sits after the layer it was copied from, which is the ordering rule"
    );
}

/// A masked recipe outlives its original's trouble: the original moved away, and then replaced by a
/// different file at the same path, each refuse the picture as `source-unavailable` — the second
/// naming the changed fingerprint — and discard nothing: the mask, its component and the layer
/// bound to it are listed exactly as they were.
#[test]
fn a_missing_or_changed_original_refuses_the_picture_and_discards_no_mask() {
    let client = Session::open("original");
    client
        .owner
        .prepare(client.client, &client.asset)
        .expect("the source is prepared");
    let mask = client.run(
        "mask.create-linear",
        &MaskTarget::default(),
        linear(0.5, 0.2, 0.5, 0.8),
        "create",
    )["mask"]
        .clone();
    client.send(
        "edit.set-basic",
        json!({"asset_id": client.asset, "mutation": mutation(client.revision(), "masked"),
               "mask": mask, "exposure": 1.0}),
    );
    let listed = client.list();
    assert_eq!(listed["masks"].as_array().unwrap().len(), 1);
    assert_eq!(listed["masks"][0]["layers"].as_array().unwrap().len(), 1);
    let sample = json!({"asset_id": client.asset, "x": 240, "y": 300});
    let source = client.dir.join("orientation-1.jpg");
    let moved = client.dir.join("moved.jpg");

    std::fs::rename(&source, &moved).expect("the original moves away");
    let (code, message) = client
        .owner
        .refused(client.client, "render.sample", sample.clone())
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(code, "source-unavailable", "{message}");
    assert_eq!(client.list(), listed, "a missing original discards nothing");

    let mut bytes = std::fs::read(&moved).expect("the original is where it was moved");
    bytes.extend_from_slice(b"a different file at the same path");
    std::fs::write(&source, bytes).expect("a different file takes its place");
    let (code, message) = client
        .owner
        .refused(client.client, "render.sample", sample)
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(code, "source-unavailable", "{message}");
    assert!(message.contains("fingerprint changed"), "{message}");
    assert_eq!(client.list(), listed, "a changed original discards nothing");
}
