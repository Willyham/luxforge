//! The setups `luxforge.ai` declares: what each of its tools needs, grouped into the parts a person
//! knows them by, with the words its setup sheet says (`docs/design/ai-tools-workspace.md`,
//! "Setting up"). A run declares them over the first generative model it offers and its
//! segmenter: the qualified models in an ordinary run, the developer fakes in a developer run.
//!
//! | Setup | Parts |
//! | --- | --- |
//! | `remove` | Generative fill |
//! | `object` | Object finder, Generative fill |
//! | `mask-object` | Object finder |
//! | `replace` | Replace |
use super::{
    analysis::models::{AnalysisModels, SEGMENTER},
    models::GenerativeModel,
};
use crate::capabilities::descriptor::{
    Introduction, IntroductionPicture, IntroductionStep, SetupDescriptor, SetupPart,
};

pub(crate) const REMOVE: &str = "remove";
pub(crate) const OBJECT: &str = "object";
pub(crate) const MASK_OBJECT: &str = "mask-object";
pub(crate) const REPLACE: &str = "replace";

/// The part names, as Settings › Models and every setup sheet name them.
const FILL_PART: &str = "Generative fill";
const FINDER_PART: &str = "Object finder";
const REPLACE_PART: &str = "Replace";

fn step(title: &str, body: &str, picture: IntroductionPicture) -> IntroductionStep {
    IntroductionStep {
        title: title.into(),
        body: body.into(),
        picture,
    }
}

/// Remove, in three steps.
fn remove_introduction() -> Introduction {
    Introduction {
        eyebrow: "AI editing".into(),
        title: "Remove anything, fill it in naturally".into(),
        lead: "Paint over something you don't want and Luxforge fills the area in to match the \
            rest of the photo. It runs entirely on this computer, so your photos never leave it."
            .into(),
        hero: None,
        steps: vec![
            step(
                "Paint over it",
                "Brush across the object, its shadow and its reflection. Cover a little more than \
                 you think.",
                IntroductionPicture::PaintOver,
            ),
            step(
                "Generate",
                "Generate makes a fill and Regenerate another. Compare them over the photo and \
                 keep the best.",
                IntroductionPicture::Candidates,
            ),
            step(
                "Accept",
                "The fill becomes one step in the photo's history. Your original file is never \
                 changed.",
                IntroductionPicture::History,
            ),
        ],
        promise: "Your photos stay on this computer.".into(),
    }
}

/// Object removal, in three steps.
fn object_introduction() -> Introduction {
    Introduction {
        eyebrow: "AI editing".into(),
        title: "Remove an object by pointing at it".into(),
        lead: "Hold the pointer over something you don't want, click, and Luxforge fills the \
            space to match the rest of the photo. It runs entirely on this computer, so your \
            photos never leave it."
            .into(),
        hero: None,
        steps: vec![
            step(
                "Point at it",
                "Move over the photo: the object under the pointer is outlined as you go.",
                IntroductionPicture::Hover,
            ),
            step(
                "Click to select",
                "Click the outlined object to select it, or drag loosely across one to find it.",
                IntroductionPicture::Selected,
            ),
            step(
                "Generate and accept",
                "Generate fills the space it leaves. Accept the fill you like and it becomes one \
                 step in the photo's history.",
                IntroductionPicture::Candidates,
            ),
        ],
        promise: "Your photos stay on this computer.".into(),
    }
}

/// Mask Object, in two steps.
fn mask_object_introduction() -> Introduction {
    Introduction {
        eyebrow: "AI editing".into(),
        title: "Select an object by pointing at it".into(),
        lead: "Hold the pointer over something in the photo and Luxforge outlines the whole \
            object; click and it becomes a mask you can adjust. It runs entirely on this \
            computer, so your photos never leave it."
            .into(),
        hero: None,
        steps: vec![
            step(
                "Point at it",
                "Move over the photo: the object under the pointer is outlined as you go.",
                IntroductionPicture::Hover,
            ),
            step(
                "Click to select",
                "Click the outlined object and it becomes the mask, or a part of the mask you are \
                 building.",
                IntroductionPicture::Selected,
            ),
        ],
        promise: "Your photos stay on this computer.".into(),
    }
}

/// Replace, the add-on that reads a prompt, in one picture.
fn replace_introduction() -> Introduction {
    Introduction {
        eyebrow: "AI editing · Add-on".into(),
        title: "Fill it with what you describe".into(),
        lead: "Type what should be there, such as \u{201c}a red bicycle\u{201d} or \u{201c}calm \
            water\u{201d}, and Replace paints it into the area you selected. It adds a second \
            model, which also runs entirely on this computer."
            .into(),
        hero: Some(IntroductionPicture::Describe),
        steps: Vec::new(),
        promise: "Your photos and what you type stay on this computer.".into(),
    }
}

/// The setups a run declares over `models` and `analysis`: `remove` and `replace` over the first
/// generative model it offers, `object` and `mask-object` over its segmenter too. A run without
/// a segmenter declares neither picking setup, and a model without a text encoder no `replace`.
pub(crate) fn declared(
    models: &[GenerativeModel],
    analysis: &AnalysisModels,
) -> Vec<SetupDescriptor> {
    let Some(model) = models.first() else {
        return Vec::new();
    };
    let fill = SetupPart {
        name: FILL_PART.into(),
        purpose: "Fills the space where something was, to match the rest of the photo".into(),
        resources: vec![model.generator.resource.into()],
    };
    let finder = analysis.set(&SEGMENTER).map(|segmenter| SetupPart {
        name: FINDER_PART.into(),
        purpose: "Finds the whole object under the pointer".into(),
        resources: segmenter
            .iter()
            .map(|model| model.file.resource.into())
            .collect(),
    });
    let mut setups = vec![SetupDescriptor {
        id: REMOVE.into(),
        title: "Remove".into(),
        introduction: remove_introduction(),
        parts: vec![fill.clone()],
    }];
    if let Some(finder) = finder {
        setups.push(SetupDescriptor {
            id: OBJECT.into(),
            title: "Object".into(),
            introduction: object_introduction(),
            parts: vec![finder.clone(), fill],
        });
        setups.push(SetupDescriptor {
            id: MASK_OBJECT.into(),
            title: "Mask Object".into(),
            introduction: mask_object_introduction(),
            parts: vec![finder],
        });
    }
    if let Some(encoder) = model.text_encoder {
        setups.push(SetupDescriptor {
            id: REPLACE.into(),
            title: "Replace".into(),
            introduction: replace_introduction(),
            parts: vec![SetupPart {
                name: REPLACE_PART.into(),
                purpose: "Reads what you describe, so Replace can fill with it".into(),
                resources: vec![encoder.resource.into()],
            }],
        });
    }
    setups
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ModuleDescriptor, ModuleRegistry, modules::ai::module::AI_MODULE};

    type Shape<'a> = Vec<(&'a str, Vec<(&'a str, Vec<&'a str>)>)>;

    /// Each setup's id and parts with their resources.
    fn shape(descriptor: &ModuleDescriptor) -> Shape<'_> {
        descriptor
            .setups
            .iter()
            .map(|setup| {
                (
                    setup.id.as_str(),
                    setup
                        .parts
                        .iter()
                        .map(|part| {
                            (
                                part.name.as_str(),
                                part.resources.iter().map(String::as_str).collect(),
                            )
                        })
                        .collect(),
                )
            })
            .collect()
    }

    fn served(developer: bool) -> ModuleDescriptor {
        let registry = if developer {
            ModuleRegistry::developer()
        } else {
            ModuleRegistry::builtin()
        };
        let descriptor = registry.module(AI_MODULE).unwrap().descriptor().clone();
        descriptor.validate().expect("the setups validate");
        descriptor
    }

    #[test]
    fn every_run_declares_remove_object_mask_object_and_replace_over_its_own_models() {
        let ordinary = served(false);
        let fill = ("Generative fill", vec!["generative-klein-4b"]);
        let finder = (
            "Object finder",
            vec!["segmenter-l0-encoder", "segmenter-l0-decoder"],
        );
        assert_eq!(
            shape(&ordinary),
            [
                (REMOVE, vec![fill.clone()]),
                (OBJECT, vec![finder.clone(), fill]),
                (MASK_OBJECT, vec![finder]),
                (
                    REPLACE,
                    vec![("Replace", vec!["generative-klein-text-encoder"])]
                ),
            ]
        );
        // What each downloads when nothing is installed, in the design's units: 2.5 GB, 2.7 GB,
        // 133 MB and 2.3 GB.
        let bytes = |id: &str| -> f64 {
            let setup = ordinary.setup(id).unwrap();
            setup
                .resources()
                .map(|resource| ordinary.resource(resource).unwrap().total_bytes())
                .sum::<u64>() as f64
        };
        let gb = |bytes: f64| (bytes / f64::from(1u32 << 30) * 10.0).round() / 10.0;
        assert_eq!(gb(bytes(REMOVE)), 2.5);
        assert_eq!(gb(bytes(OBJECT)), 2.7);
        assert_eq!((bytes(MASK_OBJECT) / f64::from(1u32 << 20)).round(), 133.0);
        assert_eq!(gb(bytes(REPLACE)), 2.3);

        let developer = served(true);
        let fill = ("Generative fill", vec!["fake-generator"]);
        let finder = (
            "Object finder",
            vec!["fake-segmenter-encoder", "fake-segmenter-decoder"],
        );
        assert_eq!(
            shape(&developer),
            [
                (REMOVE, vec![fill.clone()]),
                (OBJECT, vec![finder.clone(), fill]),
                (MASK_OBJECT, vec![finder]),
                (REPLACE, vec![("Replace", vec!["fake-text-encoder"])]),
            ]
        );
    }

    #[test]
    fn each_setup_says_its_own_words() {
        let descriptor = served(false);
        let introduction = |id: &str| descriptor.setup(id).unwrap().introduction.clone();
        assert_eq!(
            introduction(REMOVE).title,
            "Remove anything, fill it in naturally"
        );
        assert_eq!(
            introduction(REPLACE).title,
            "Fill it with what you describe"
        );
        let object = introduction(OBJECT);
        assert_eq!(object.title, "Remove an object by pointing at it");
        assert_eq!(
            object
                .steps
                .iter()
                .map(|step| (step.title.as_str(), step.picture))
                .collect::<Vec<_>>(),
            [
                ("Point at it", IntroductionPicture::Hover),
                ("Click to select", IntroductionPicture::Selected),
                ("Generate and accept", IntroductionPicture::Candidates),
            ]
        );
        let masks = introduction(MASK_OBJECT);
        assert_eq!(masks.title, "Select an object by pointing at it");
        assert_eq!(
            masks
                .steps
                .iter()
                .map(|step| step.picture)
                .collect::<Vec<_>>(),
            [IntroductionPicture::Hover, IntroductionPicture::Selected]
        );
        // A download introduces itself by the smallest setup that holds it.
        let by = |resource: &str| {
            crate::capabilities::setups::introducing(&descriptor, resource)
                .map(|setup| setup.id.as_str())
        };
        assert_eq!(by("generative-klein-4b"), Some(REMOVE));
        assert_eq!(by("segmenter-l0-encoder"), Some(MASK_OBJECT));
        assert_eq!(by("generative-klein-text-encoder"), Some(REPLACE));
    }
}
