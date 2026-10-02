//! The GPU preview qualification corpus: `fixtures/preview/corpus.json`, the sources and recipes
//! the preview error limits are judged on, each at Fit and at 100%. This module reads it and holds
//! it to its own rules; it measures nothing.
//!
//! A source is a generated JPEG, a RAW photograph or an explicit gap. A generated JPEG names its
//! file under `fixtures/generated/` and the SHA-256 `cargo xtask generate-fixtures` writes it with.
//! A RAW source names its id in the private RAW manifest (`--manifest FILE`, read through the one
//! reader, [`raw::manifest`]) and repeats that entry's SHA-256, so a manifest that has moved on is
//! noticed. A source with no hash carries a `gap` that says why, never a made-up hash. A recipe is
//! a list of evidence-script steps over the sources it names, in a class that says which limits it
//! is held to.
//!
//! [`Corpus::load`] checks the file's own consistency (every required source and recipe family is
//! present; every step is a step the evidence driver parses and every call is one the committed
//! descriptors declare), which `cargo test` runs on every build. [`run`] (`cargo xtask
//! preview-corpus`) goes further on a host that has the files: it re-hashes each source and, with
//! `--manifest`, resolves the RAW entries. A source it could not check on this host is
//! `unresolved`, which makes the run incomplete (its own exit code), never a pass.
use crate::{raw, *};
use luxforge_reference::preview_error::Class;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

pub const FILE: &str = "fixtures/preview/corpus.json";
/// The committed snapshot of every method's declared parameters, which a recipe's calls are
/// checked against.
const DESCRIPTORS: &str = "fixtures/modules/builtin-descriptors.json";

/// The views every recipe is measured at.
const VIEWS: [&str; 2] = ["fit", "100%"];

/// The sources the design names; the corpus must hold each.
const REQUIRED_SOURCES: [&str; 7] = [
    "jpeg-24mp",
    "jpeg-60mp",
    "zone-plate",
    "presence-fixture",
    "raw-z6",
    "raw-x100vi",
    "raw-air2s",
];

/// Every recipe family and the class its limits come from: pointwise for the colour units, masks,
/// the vignette and geometry under a colour edit, spatial for Presence and Detail.
const FAMILIES: [(&str, Class); 15] = [
    ("basic", Class::Pointwise),
    ("tone-curve", Class::Pointwise),
    ("mixer", Class::Pointwise),
    ("vignette", Class::Pointwise),
    ("colour-stack", Class::Pointwise),
    ("mask-linear", Class::Pointwise),
    ("mask-radial", Class::Pointwise),
    ("mask-brush", Class::Pointwise),
    ("mask-luminance-range", Class::Pointwise),
    ("mask-colour-range", Class::Pointwise),
    ("mask-composed", Class::Pointwise),
    ("crop", Class::Pointwise),
    ("lens-perspective", Class::Pointwise),
    ("presence", Class::Spatial),
    ("detail", Class::Spatial),
];

/// The families the design lists, which the corpus must hold at least one recipe of. The component
/// algebra's `mask-composed` and the four colour recipes together, `colour-stack`, are recipes the
/// corpus adds beyond them.
const REQUIRED_FAMILIES: [&str; 13] = [
    "basic",
    "tone-curve",
    "mixer",
    "vignette",
    "mask-linear",
    "mask-radial",
    "mask-brush",
    "mask-luminance-range",
    "mask-colour-range",
    "crop",
    "lens-perspective",
    "presence",
    "detail",
];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Corpus {
    format: u32,
    scope: String,
    views: Vec<View>,
    sources: Vec<Source>,
    recipes: Vec<Recipe>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct View {
    id: String,
    step: Value,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum Kind {
    GeneratedJpeg,
    Raw,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::GeneratedJpeg => "generated-jpeg",
            Self::Raw => "raw",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    id: String,
    kind: Kind,
    description: String,
    dimensions: [u32; 2],
    /// A generated JPEG's file, relative to the checkout.
    path: Option<String>,
    /// The command that writes it.
    generator: Option<String>,
    /// A RAW source's id in the private RAW manifest.
    manifest_id: Option<String>,
    sha256: Option<String>,
    /// Why the source has no hash.
    gap: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Recipe {
    id: String,
    family: String,
    class: String,
    description: String,
    sources: Vec<String>,
    steps: Vec<Value>,
}

fn lowercase_hex_sha256(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl Corpus {
    /// The committed corpus, read and checked against the committed descriptors.
    pub fn load(root: &Path) -> Result<Self> {
        let corpus: Self = serde_json::from_slice(&fs::read(root.join(FILE))?)?;
        corpus.check(&read_json(&root.join(DESCRIPTORS))?)?;
        Ok(corpus)
    }

    /// How many (recipe, source, view) cells the corpus lists.
    pub fn cells(&self) -> usize {
        self.recipes
            .iter()
            .map(|recipe| recipe.sources.len())
            .sum::<usize>()
            * self.views.len()
    }

    /// The corpus's own consistency, against `descriptors` (the `fixtures/modules` snapshot).
    pub fn check(&self, descriptors: &Value) -> Result {
        ensure(self.format == 1, "The corpus's format is 1")?;
        ensure(!self.scope.trim().is_empty(), "The corpus states no scope")?;
        ensure(
            self.views.iter().map(|view| view.id.as_str()).eq(VIEWS),
            format!("The corpus's views are {VIEWS:?}, each at Fit and at 100%"),
        )?;
        for view in &self.views {
            let steps = luxforge_evidence::parse(&json!([view.step]).to_string())?;
            ensure(
                matches!(steps.as_slice(), [luxforge_evidence::Step::View(_)]),
                format!("The {} view is not a view step", view.id),
            )?;
        }

        let mut ids = HashSet::new();
        for source in &self.sources {
            let id = &source.id;
            ensure(ids.insert(id.as_str()), format!("Duplicate source {id}"))?;
            ensure(
                !source.description.trim().is_empty()
                    && source.dimensions.iter().all(|side| *side > 0),
                format!("Source {id} needs a description and dimensions"),
            )?;
            match (&source.sha256, &source.gap) {
                (Some(hash), None) => ensure(
                    lowercase_hex_sha256(hash),
                    format!("Source {id} needs a lowercase SHA-256"),
                )?,
                (None, Some(gap)) => ensure(
                    !gap.trim().is_empty(),
                    format!("Source {id} records an empty gap"),
                )?,
                _ => {
                    return Err(format!(
                        "Source {id} carries its SHA-256 or an explicit gap, one of the two"
                    )
                    .into());
                }
            }
            let located = match source.kind {
                Kind::GeneratedJpeg => {
                    source.manifest_id.is_none()
                        && match (&source.path, &source.generator) {
                            (Some(path), Some(generator)) => {
                                path.starts_with("fixtures/generated/")
                                    && !path.contains("..")
                                    && !generator.trim().is_empty()
                            }
                            // A gap needs no file yet, but then it cannot name half of one.
                            (None, None) => source.gap.is_some(),
                            _ => false,
                        }
                }
                Kind::Raw => {
                    source.path.is_none()
                        && source.generator.is_none()
                        && source
                            .manifest_id
                            .as_deref()
                            .is_some_and(|manifest_id| !manifest_id.is_empty())
                }
            };
            ensure(
                located,
                format!(
                    "Source {id} does not locate its file as a {} source does",
                    source.kind.name()
                ),
            )?;
        }
        for required in REQUIRED_SOURCES {
            ensure(
                ids.contains(required),
                format!("The corpus lists no {required} source"),
            )?;
        }

        let methods = &descriptors["schema.list"]["methods"];
        ensure(
            methods.is_object(),
            "The descriptors snapshot lists no methods",
        )?;
        let families: HashMap<&str, Class> = FAMILIES.into_iter().collect();
        let mut recipe_ids = HashSet::new();
        let mut used = HashSet::new();
        for recipe in &self.recipes {
            let id = &recipe.id;
            ensure(
                recipe_ids.insert(id.as_str()),
                format!("Duplicate recipe {id}"),
            )?;
            ensure(
                !recipe.description.trim().is_empty(),
                format!("Recipe {id} needs a description"),
            )?;
            let family = families
                .get(recipe.family.as_str())
                .ok_or_else(|| format!("Recipe {id} is in an unknown family {}", recipe.family))?;
            ensure(
                Class::parse(&recipe.class) == Some(*family),
                format!(
                    "Recipe {id} is {}, but its family {} is held to the {} limits",
                    recipe.class,
                    recipe.family,
                    family.name()
                ),
            )?;
            ensure(
                !recipe.sources.is_empty(),
                format!("Recipe {id} applies to no source"),
            )?;
            let mut named = HashSet::new();
            for source in &recipe.sources {
                ensure(
                    ids.contains(source.as_str()),
                    format!("Recipe {id} names an unknown source {source}"),
                )?;
                ensure(
                    named.insert(source.as_str()),
                    format!("Recipe {id} names {source} twice"),
                )?;
                used.insert(source.as_str());
            }
            ensure(
                !recipe.steps.is_empty(),
                format!("Recipe {id} has no steps"),
            )?;
            luxforge_evidence::parse(&Value::Array(recipe.steps.clone()).to_string())
                .map_err(|error| format!("Recipe {id}: {error}"))?;
            for step in &recipe.steps {
                if let Some(call) = step.get("api") {
                    check_call(id, call, methods)?;
                }
            }
        }
        for family in REQUIRED_FAMILIES {
            ensure(
                self.recipes.iter().any(|recipe| recipe.family == family),
                format!("The corpus holds no {family} recipe"),
            )?;
        }
        for source in &self.sources {
            ensure(
                used.contains(source.id.as_str()),
                format!("No recipe is measured over source {}", source.id),
            )?;
        }
        Ok(())
    }

    /// Each source as this host can resolve it. `root` is the checkout; `manifest` the private RAW
    /// manifest, when given. A source whose hash disagrees with what is found is an error; one
    /// that cannot be checked here is `unresolved` with the reason.
    pub fn resolve(&self, root: &Path, manifest: Option<&Path>) -> Result<Value> {
        let manifest = manifest
            .map(raw::manifest::<raw::EditorSource>)
            .transpose()?;
        let mut sources = Vec::new();
        let (mut verified, mut gaps, mut unresolved) = (0, 0, 0);
        for source in &self.sources {
            let id = &source.id;
            let (status, detail) = match (&source.sha256, &source.gap) {
                (None, Some(gap)) => ("gap", gap.clone()),
                (Some(hash), _) => {
                    let found = match source.kind {
                        Kind::GeneratedJpeg => {
                            let path = root.join(source.path.as_deref().unwrap_or_default());
                            if path.is_file() {
                                Ok(path)
                            } else {
                                Err(format!(
                                    "{} is not generated on this host (cargo xtask generate-fixtures)",
                                    path.display()
                                ))
                            }
                        }
                        Kind::Raw => match &manifest {
                            None => {
                                Err("no --manifest, so the RAW entry is not resolved".to_owned())
                            }
                            Some(manifest) => {
                                let manifest_id = source.manifest_id.as_deref().unwrap_or_default();
                                let (entry, path) = manifest
                                    .located()
                                    .find(|(entry, _)| entry.id == manifest_id)
                                    .ok_or_else(|| {
                                        format!(
                                            "The manifest lists no source {manifest_id} for {id}"
                                        )
                                    })?;
                                ensure(
                                    entry.sha256 == *hash,
                                    format!(
                                        "Source {id}: the manifest's {manifest_id} has SHA-256 {}, the corpus {hash}",
                                        entry.sha256
                                    ),
                                )?;
                                if path.is_file() {
                                    Ok(path)
                                } else {
                                    Err(format!("{} is not on this host", path.display()))
                                }
                            }
                        },
                    };
                    match found {
                        Ok(path) => {
                            let actual = crate::hash(&path)?;
                            ensure(
                                actual == *hash,
                                format!(
                                    "Source {id}: {} has SHA-256 {actual}, the corpus {hash}",
                                    path.display()
                                ),
                            )?;
                            ("verified", format!("SHA-256 of {} matches", path.display()))
                        }
                        Err(reason) => ("unresolved", reason),
                    }
                }
                (None, None) => {
                    return Err(format!("Source {id} has neither a hash nor a gap").into());
                }
            };
            match status {
                "verified" => verified += 1,
                "gap" => gaps += 1,
                _ => unresolved += 1,
            }
            sources.push(
                json!({"id": id, "kind": source.kind.name(), "status": status, "detail": detail}),
            );
        }
        Ok(json!({
            "corpus": FILE,
            "views": self.views.iter().map(|view| &view.id).collect::<Vec<_>>(),
            "recipes": self.recipes.len(),
            "cells": self.cells(),
            "entries": sources,
            "counts": {"verified": verified, "gap": gaps, "unresolved": unresolved},
        }))
    }
}

/// One recipe call against the committed descriptors: a method that exists, only parameters it
/// declares and every one it requires.
fn check_call(recipe: &str, call: &Value, methods: &Value) -> Result {
    let method = call["method"].as_str().unwrap_or_default();
    let declared = methods
        .get(method)
        .ok_or_else(|| format!("Recipe {recipe} calls {method}, which no module declares"))?;
    let params = call["params"].as_object().cloned().unwrap_or_default();
    let listed = declared["parameters"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let known: HashSet<&str> = listed
        .iter()
        .filter_map(|parameter| parameter["name"].as_str())
        .chain(
            declared["optional"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(name, _)| name.as_str()),
        )
        .collect();
    for name in params.keys() {
        ensure(
            known.contains(name.as_str()),
            format!("Recipe {recipe}: {method} declares no parameter {name}"),
        )?;
    }
    for parameter in &listed {
        let name = parameter["name"].as_str().unwrap_or_default();
        // The descriptors list a parameter with a default as required too; leaving it out takes
        // the default, as `mask_acceptance`'s own strokes do.
        let needed = parameter["required"] == json!(true) && parameter["default"].is_null();
        ensure(
            !needed || params.contains_key(name),
            format!("Recipe {recipe}: {method} requires {name}"),
        )?;
    }
    Ok(())
}

/// `preview-corpus [--manifest FILE] [--output NEW_FILE]`: check the corpus and resolve what this
/// host can. Incomplete (exit 3) when any source could not be checked here.
pub fn run(root: &Path, mut a: Args) -> Result {
    let manifest = a
        .value("--manifest")?
        .map(|path| absolute(root, Path::new(&path)));
    let output = a.value("--output")?.map(PathBuf::from);
    if let Some(output) = &output {
        ensure(
            !output.exists(),
            format!("{} exists: use a new file", output.display()),
        )?;
    }
    a.done()?;
    let corpus = Corpus::load(root)?;
    let report = corpus.resolve(root, manifest.as_deref())?;
    let text = format!("{}\n", serde_json::to_string_pretty(&report)?);
    if let Some(output) = &output {
        fs::write(output, &text)?;
    }
    print!("{text}");
    let unresolved: Vec<String> = report["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|source| source["status"] == "unresolved")
        .map(|source| {
            format!(
                "{}: {}",
                source["id"].as_str().unwrap_or_default(),
                source["detail"].as_str().unwrap_or_default()
            )
        })
        .collect();
    if unresolved.is_empty() {
        Ok(())
    } else {
        Err(Box::new(crate::verify::Incomplete(format!(
            "{} source(s) could not be checked on this host, so the corpus is not fully verified: {}",
            unresolved.len(),
            unresolved.join("; ")
        ))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn committed() -> (PathBuf, Value) {
        let root = root().unwrap();
        let value = read_json(&root.join(FILE)).unwrap();
        (root, value)
    }

    fn check(corpus: &Value) -> Result {
        let (root, _) = committed();
        let corpus: Corpus = serde_json::from_value(corpus.clone())?;
        corpus.check(&read_json(&root.join(DESCRIPTORS))?)
    }

    /// The committed corpus with one change, and the refusal it produces.
    fn refused(change: impl FnOnce(&mut Value)) -> String {
        let (_, mut corpus) = committed();
        change(&mut corpus);
        check(&corpus).unwrap_err().to_string()
    }

    fn recipe<'a>(corpus: &'a mut Value, id: &str) -> &'a mut Value {
        corpus["recipes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|recipe| recipe["id"] == id)
            .unwrap()
    }

    fn source<'a>(corpus: &'a mut Value, id: &str) -> &'a mut Value {
        corpus["sources"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|source| source["id"] == id)
            .unwrap()
    }

    #[test]
    fn preview_error_corpus_lists_every_source_and_recipe() {
        let (root, value) = committed();
        let corpus = Corpus::load(&root).unwrap();
        // Every source the design names, each hashed or an explicit gap, never both.
        let sources: HashMap<&str, &Source> =
            corpus.sources.iter().map(|s| (s.id.as_str(), s)).collect();
        for id in REQUIRED_SOURCES {
            let source = sources[id];
            assert_eq!(source.sha256.is_some(), source.gap.is_none(), "{id}");
        }
        // Every source has a file: the zone plate is generated with the other JPEGs.
        let gaps: Vec<&str> = corpus
            .sources
            .iter()
            .filter(|s| s.gap.is_some())
            .map(|s| s.id.as_str())
            .collect();
        assert!(gaps.is_empty(), "{gaps:?}");
        // The RAWs resolve through the private manifest by the ids the editor manifest uses.
        let raws: Vec<(&str, &str)> = corpus
            .sources
            .iter()
            .filter(|s| s.kind == Kind::Raw)
            .map(|s| (s.id.as_str(), s.manifest_id.as_deref().unwrap()))
            .collect();
        assert_eq!(
            raws,
            [
                ("raw-z6", "nikon-z6"),
                ("raw-x100vi", "fujifilm-x100vi"),
                ("raw-air2s", "dji-air2s")
            ]
        );
        // Every recipe family the design lists, in its class, and every cell counted.
        for family in REQUIRED_FAMILIES {
            assert!(
                corpus.recipes.iter().any(|r| r.family == family),
                "{family}"
            );
        }
        let expected: usize = value["recipes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|recipe| recipe["sources"].as_array().unwrap().len() * 2)
            .sum();
        assert_eq!(corpus.cells(), expected);
        assert_eq!(
            corpus
                .views
                .iter()
                .map(|v| v.id.as_str())
                .collect::<Vec<_>>(),
            VIEWS
        );
        // Each of the five mask kinds is its own recipe, and the Detail recipes are the study's
        // three parameter sets, read from its own corpus rather than copied.
        let detail = read_json(&root.join("fixtures/detail/corpus.json")).unwrap();
        for set in detail["parameter_sets"].as_array().unwrap() {
            let id = format!("detail-{}", set["id"].as_str().unwrap());
            let recipe = corpus
                .recipes
                .iter()
                .find(|r| r.id == id)
                .unwrap_or_else(|| panic!("{id}"));
            assert_eq!(recipe.steps.len(), 1);
            assert_eq!(recipe.steps[0]["api"]["params"], set["parameters"], "{id}");
        }
    }

    #[test]
    fn preview_error_corpus_refuses_a_source_that_is_neither_hashed_nor_a_gap() {
        assert!(
            refused(|c| {
                source(c, "jpeg-24mp")
                    .as_object_mut()
                    .unwrap()
                    .remove("sha256");
            })
            .contains("its SHA-256 or an explicit gap")
        );
        assert!(
            refused(|c| {
                source(c, "zone-plate")["gap"] = json!("both a hash and a gap");
            })
            .contains("its SHA-256 or an explicit gap")
        );
        assert!(
            refused(|c| {
                let zone = source(c, "zone-plate").as_object_mut().unwrap();
                zone.remove("sha256");
                zone.insert("gap".into(), json!(" "));
            })
            .contains("empty gap")
        );
        assert!(
            refused(|c| {
                source(c, "raw-z6")["sha256"] = json!("A".repeat(64));
            })
            .contains("lowercase SHA-256")
        );
        assert!(
            refused(|c| {
                source(c, "jpeg-60mp")["sha256"] = json!("abc");
            })
            .contains("lowercase SHA-256")
        );
        // A source locates its file as its kind does.
        assert!(
            refused(|c| {
                source(c, "raw-z6")
                    .as_object_mut()
                    .unwrap()
                    .remove("manifest_id");
            })
            .contains("does not locate")
        );
        assert!(
            refused(|c| {
                source(c, "raw-z6")["path"] = json!("fixtures/raw/z6.NEF");
            })
            .contains("does not locate")
        );
        assert!(
            refused(|c| {
                source(c, "jpeg-24mp")["path"] = json!("../private/24mp.jpg");
            })
            .contains("does not locate")
        );
        assert!(
            refused(|c| {
                source(c, "jpeg-24mp")
                    .as_object_mut()
                    .unwrap()
                    .remove("generator");
            })
            .contains("does not locate")
        );
        // Every source the design names, and every one used.
        assert!(
            refused(|c| {
                c["sources"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|s| s["id"] != "raw-air2s");
                for recipe in c["recipes"].as_array_mut().unwrap() {
                    recipe["sources"]
                        .as_array_mut()
                        .unwrap()
                        .retain(|s| s != "raw-air2s");
                }
            })
            .contains("no raw-air2s source")
        );
        assert!(
            refused(|c| {
                for recipe in c["recipes"].as_array_mut().unwrap() {
                    recipe["sources"]
                        .as_array_mut()
                        .unwrap()
                        .retain(|s| s != "zone-plate");
                }
            })
            .contains("No recipe is measured over source zone-plate")
        );
        assert!(
            refused(|c| {
                let first = c["sources"][0].clone();
                c["sources"].as_array_mut().unwrap().push(first);
            })
            .contains("Duplicate source")
        );
    }

    #[test]
    fn preview_error_corpus_refuses_a_recipe_it_cannot_run_or_judge() {
        assert!(
            refused(|c| {
                recipe(c, "tone-curve")["steps"][0]["api"]["method"] = json!("edit.set-tones");
            })
            .contains("no module declares")
        );
        assert!(
            refused(|c| {
                recipe(c, "vignette")["steps"][0]["api"]["params"]["strength"] = json!(1);
            })
            .contains("declares no parameter strength")
        );
        assert!(
            refused(|c| {
                recipe(c, "mask-linear")["steps"][0]["api"]["params"]
                    .as_object_mut()
                    .unwrap()
                    .remove("y1");
            })
            .contains("requires y1")
        );
        assert!(
            refused(|c| {
                recipe(c, "mixer")["steps"] = json!([{"nope": 1}]);
            })
            .contains("Recipe mixer")
        );
        assert!(
            refused(|c| {
                recipe(c, "mixer")["steps"] = json!([]);
            })
            .contains("has no steps")
        );
        assert!(
            refused(|c| {
                recipe(c, "mixer")["sources"] = json!(["nowhere"]);
            })
            .contains("unknown source nowhere")
        );
        assert!(
            refused(|c| {
                recipe(c, "mixer")["sources"] = json!(["jpeg-24mp", "jpeg-24mp"]);
            })
            .contains("twice")
        );
        assert!(
            refused(|c| {
                recipe(c, "mixer")["sources"] = json!([]);
            })
            .contains("applies to no source")
        );
        // A class the family is not held to, and a family nobody defined.
        assert!(
            refused(|c| {
                recipe(c, "presence-all")["class"] = json!("pointwise");
            })
            .contains("held to the spatial limits")
        );
        assert!(
            refused(|c| {
                recipe(c, "mixer")["family"] = json!("sepia");
            })
            .contains("unknown family")
        );
        // Every family the design lists keeps a recipe.
        assert!(
            refused(|c| {
                c["recipes"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|r| r["family"] != "detail");
            })
            .contains("no detail recipe")
        );
        assert!(
            refused(|c| {
                let first = c["recipes"][0].clone();
                c["recipes"].as_array_mut().unwrap().push(first);
            })
            .contains("Duplicate recipe")
        );
        // Both views, in order, as steps the driver reads.
        assert!(
            refused(|c| {
                c["views"].as_array_mut().unwrap().pop();
            })
            .contains("views are")
        );
        assert!(
            refused(|c| {
                c["views"][1]["step"] = json!({"wait": {"ms": 5}});
            })
            .contains("not a view step")
        );
        assert!(
            refused(|c| {
                c["format"] = json!(2);
            })
            .contains("format is 1")
        );
        let (_, mut unknown) = committed();
        unknown["extra"] = json!(true);
        assert!(check(&unknown).is_err(), "an unknown field is refused");
    }

    /// The baseline journeys the performance document's error figures were taken with are scripts
    /// the editor's driver reads, whose calls the committed descriptors declare.
    #[test]
    fn preview_error_baseline_scripts_are_evidence_scripts_over_declared_methods() {
        let (root, _) = committed();
        let descriptors = read_json(&root.join(DESCRIPTORS)).unwrap();
        for (file, steps) in [("presence-fit.json", 18), ("region-100.json", 4)] {
            let text =
                fs::read_to_string(root.join("fixtures/preview/baseline").join(file)).unwrap();
            let parsed = luxforge_evidence::parse(&text).unwrap();
            assert_eq!(parsed.len(), steps, "{file}");
            for step in serde_json::from_str::<Vec<Value>>(&text).unwrap() {
                if let Some(call) = step.get("api") {
                    check_call(file, call, &descriptors["schema.list"]["methods"]).unwrap();
                }
            }
        }
    }

    fn corpus_of(sources: Value) -> Corpus {
        serde_json::from_value(json!({
            "format": 1, "scope": "s", "views": [], "recipes": [], "sources": sources,
        }))
        .unwrap()
    }

    #[test]
    fn preview_error_corpus_resolves_hashes_gaps_and_raw_entries_through_the_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("fixtures/generated")).unwrap();
        fs::write(root.join("fixtures/generated/a.jpg"), b"generated").unwrap();
        fs::write(root.join("photo.NEF"), b"a raw file").unwrap();
        let (generated, raw_hash) = (
            crate::hash(&root.join("fixtures/generated/a.jpg")).unwrap(),
            crate::hash(&root.join("photo.NEF")).unwrap(),
        );
        let corpus = corpus_of(json!([
            {"id": "gen", "kind": "generated-jpeg", "description": "d", "dimensions": [1, 1],
             "path": "fixtures/generated/a.jpg", "generator": "g", "sha256": generated},
            {"id": "missing", "kind": "generated-jpeg", "description": "d", "dimensions": [1, 1],
             "path": "fixtures/generated/b.jpg", "generator": "g", "sha256": "b".repeat(64)},
            {"id": "plate", "kind": "generated-jpeg", "description": "d", "dimensions": [1, 1],
             "gap": "no generator"},
            {"id": "raw", "kind": "raw", "description": "d", "dimensions": [4024, 6048],
             "manifest_id": "nikon-z6", "sha256": raw_hash},
        ]));
        let write_manifest = |path: &Path, hash: &str, id: &str| {
            let manifest = root.join("manifest.json");
            write_json(
                &manifest,
                &json!({"format": 1, "sources": [{
                    "id": id, "path": path, "sha256": hash, "mode": "NikonZ6Lossless14",
                    "make": "Nikon", "model": "Z 6", "source_dimensions": [4024, 6048],
                    "orientation": 8, "neutral_point": [1609, 2419]}]}),
            )
            .unwrap();
            manifest
        };
        let status = |report: &Value, id: &str| {
            report["entries"]
                .as_array()
                .unwrap()
                .iter()
                .find(|s| s["id"] == id)
                .unwrap()["status"]
                .clone()
        };

        // Without a manifest the RAW is unresolved, never verified, and a file not generated here
        // is too. A gap is a recorded gap.
        let report = corpus.resolve(root, None).unwrap();
        assert_eq!(
            [
                status(&report, "gen"),
                status(&report, "missing"),
                status(&report, "plate"),
                status(&report, "raw")
            ],
            ["verified", "unresolved", "gap", "unresolved"]
        );
        assert_eq!(
            report["counts"],
            json!({"verified": 1, "gap": 1, "unresolved": 2})
        );
        assert!(
            report["entries"][3]["detail"]
                .as_str()
                .unwrap()
                .contains("no --manifest")
        );

        // Through the manifest the RAW resolves: the entry's hash, then the file's.
        let manifest = write_manifest(&root.join("photo.NEF"), &raw_hash, "nikon-z6");
        let report = corpus.resolve(root, Some(&manifest)).unwrap();
        assert_eq!(status(&report, "raw"), "verified");
        // A RAW whose file is not on this host is unresolved, not an error.
        let manifest = write_manifest(&root.join("absent.NEF"), &raw_hash, "nikon-z6");
        assert_eq!(
            status(&corpus.resolve(root, Some(&manifest)).unwrap(), "raw"),
            "unresolved"
        );
        // A manifest that has moved on, an entry that is not listed and a file that changed are
        // refused rather than resolved.
        let manifest = write_manifest(&root.join("photo.NEF"), &"c".repeat(64), "nikon-z6");
        assert!(
            corpus
                .resolve(root, Some(&manifest))
                .unwrap_err()
                .to_string()
                .contains("manifest's nikon-z6 has SHA-256")
        );
        let manifest = write_manifest(&root.join("photo.NEF"), &raw_hash, "other");
        assert!(
            corpus
                .resolve(root, Some(&manifest))
                .unwrap_err()
                .to_string()
                .contains("lists no source nikon-z6")
        );
        fs::write(
            root.join("fixtures/generated/a.jpg"),
            b"regenerated differently",
        )
        .unwrap();
        assert!(
            corpus
                .resolve(root, None)
                .unwrap_err()
                .to_string()
                .contains("has SHA-256")
        );
    }

    #[test]
    fn preview_error_corpus_command_is_incomplete_not_a_pass_when_a_source_is_unresolved() {
        // A checkout holding only the corpus and its descriptors: nothing generated, no manifest.
        let (real, _) = committed();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for file in [FILE, DESCRIPTORS] {
            fs::create_dir_all(root.join(file).parent().unwrap()).unwrap();
            fs::copy(real.join(file), root.join(file)).unwrap();
        }
        let corpus = Corpus::load(root).unwrap();
        let report = corpus.resolve(root, None).unwrap();
        assert_eq!(
            report["counts"],
            json!({"verified": 0, "gap": 0, "unresolved": 7}),
            "four generated files and three RAWs are not checked here"
        );
        let error = run(root, Args(vec![])).unwrap_err();
        assert!(
            error.downcast_ref::<crate::verify::Incomplete>().is_some(),
            "{error}"
        );
        assert!(
            error
                .to_string()
                .contains("7 source(s) could not be checked"),
            "{error}"
        );
        assert!(run(root, Args(vec!["--surprise".into()])).is_err());
    }
}
