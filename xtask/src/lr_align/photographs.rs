//! A round's RAW photographs on Luxforge's side of `lr-align measure`: each one imported into a
//! scratch catalog under the evidence directory and developed by the editor service, the one public
//! route from a RAW file to the reference renderer's linear source.
//!
//! The file read is the round's own copy of the photograph beside its neutral variant, checked
//! against the manifest's SHA-256 before it is opened and again when the run ends; nothing in the
//! round folder is written. A development is the service's: As Shot at import, a custom white
//! balance through `set-raw` and its preparation, or the detected lens profile through
//! `select-lens-profile`. Each is kept for the run, so the renders that share it run in parallel
//! over one set of planes; the service itself is used by one caller at a time.
use crate::*;
use luxforge_core::{
    AssetId, EditorService, EntryId, LENS_EFFECT, Layer, LinearImage, Mutation, PreviewSource,
    Recipe,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// How a photograph is developed for one render.
#[derive(Clone, Debug, PartialEq)]
pub enum Development {
    AsShot,
    /// A custom white balance on Luxforge's own scale, Kelvin and tint.
    WhiteBalance {
        temperature: f64,
        tint: f64,
    },
    /// As Shot with the detected lens profile applied.
    Lens,
}

impl Development {
    fn key(&self) -> String {
        match self {
            Self::AsShot => "as-shot".into(),
            Self::WhiteBalance { temperature, tint } => format!("wb {temperature:.4} {tint:.4}"),
            Self::Lens => "lens".into(),
        }
    }
}

/// One development: its linear planes and the layers every render of it starts from (the RAW
/// layer, and the lens layer for [`Development::Lens`]).
pub struct Developed {
    pub image: LinearImage,
    pub base: Recipe,
}

struct Service {
    service: EditorService,
    asset: AssetId,
    revision: u64,
    requests: u64,
}

/// One photograph of the round.
pub struct Photo {
    pub path: PathBuf,
    pub sha256: String,
    pub width: u32,
    pub height: u32,
    /// The detected lens profile's key, when the service finds an eligible one.
    pub lens: Option<String>,
    service: Mutex<Service>,
    developed: Mutex<HashMap<String, Arc<Developed>>>,
}

fn layers_of(recipe: &Recipe, keep: impl Fn(&Layer) -> bool) -> Recipe {
    let mut base = recipe.clone();
    let mut first = true;
    base.layers.retain(|layer| {
        let raw = std::mem::take(&mut first);
        raw || keep(layer)
    });
    base.masks.clear();
    base
}

impl Service {
    fn mutation(&mut self) -> Mutation {
        self.requests += 1;
        Mutation {
            expected_revision: self.revision,
            request_id: format!("lr-align-{}", self.requests),
            actor: "lr-align".into(),
        }
    }

    fn apply(&mut self, action: &str, parameters: Value) -> Result<EntryId> {
        let mutation = self.mutation();
        let result = self
            .service
            .apply_action(&self.asset, mutation, action, parameters)?;
        self.revision = result.revision;
        Ok(result.current_entry_id)
    }

    /// The development `entry` evaluates, prepared first, and its full recipe.
    fn develop(&mut self, entry: &EntryId) -> Result<(LinearImage, Recipe)> {
        let needs = self.service.entry_needs(&self.asset, Some(entry))?;
        self.service.prepare(&needs)?;
        let job = self
            .service
            .preview_job(&self.asset, Some(entry), None, None, None)?;
        let PreviewSource::Raw { image, settings } = job.evaluation.source() else {
            return Err("a RAW photograph was not developed as a RAW source".into());
        };
        ensure(
            settings.white_balance.is_none(),
            "the development holds an approximate white balance",
        )?;
        Ok((
            image.clone(),
            job.evaluation.entry().snapshot.recipe.clone(),
        ))
    }
}

impl Photo {
    /// Import `path`, the round's copy of the photograph whose SHA-256 is `sha256`, into a
    /// scratch catalog in `catalog`, a new folder; develop it As Shot and with its detected lens
    /// profile when it has one.
    pub fn open(path: &Path, sha256: &str, catalog: &Path) -> Result<Self> {
        ensure(
            hash(path)? == sha256,
            format!(
                "{} is not the photograph the manifest recorded",
                path.display()
            ),
        )?;
        fs::create_dir_all(catalog)?;
        let mut service = EditorService::open(&catalog.join("catalog.sqlite"))?;
        // The rounds' baseline turns Lightroom's lens profile off; Luxforge's is applied only by
        // the lens-profile family.
        service.set_auto_lens_profile(false);
        let state = service.import(path)?;
        let mut service = Service {
            service,
            asset: state.asset.id.clone(),
            revision: state.revision,
            requests: 0,
        };
        let original = state.current_entry.id.clone();
        let (image, recipe) = service.develop(&original)?;
        let (width, height) = (image.width(), image.height());
        let mut developed = HashMap::new();
        developed.insert(
            Development::AsShot.key(),
            Arc::new(Developed {
                image,
                base: layers_of(&recipe, |_| false),
            }),
        );
        let query =
            service
                .service
                .run_query(&service.asset, &original, "lens-profiles", json!({}))?;
        let lens = Some(&query["status"]["detected"])
            .filter(|detected| detected["eligible"] == true)
            .and_then(|detected| detected["key"].as_str().map(str::to_owned));
        if let Some(key) = &lens {
            let entry = service.apply("select-lens-profile", json!({"profile": key}))?;
            let (image, recipe) = service.develop(&entry)?;
            developed.insert(
                Development::Lens.key(),
                Arc::new(Developed {
                    image,
                    base: layers_of(&recipe, |layer| layer.effect_id == LENS_EFFECT),
                }),
            );
        }
        Ok(Self {
            path: path.to_owned(),
            sha256: sha256.to_owned(),
            width,
            height,
            lens,
            service: Mutex::new(service),
            developed: Mutex::new(developed),
        })
    }

    /// The development `development` names, made once and kept.
    pub fn develop(
        &self,
        development: &Development,
    ) -> std::result::Result<Arc<Developed>, String> {
        let key = development.key();
        if let Some(done) = self
            .developed
            .lock()
            .map_err(|_| "a poisoned cache")?
            .get(&key)
        {
            return Ok(done.clone());
        }
        let Development::WhiteBalance { temperature, tint } = development else {
            return Err(format!("no {key} development of this photograph"));
        };
        let mut service = self.service.lock().map_err(|_| "a poisoned service")?;
        // Another worker may have made it while this one waited.
        if let Some(done) = self
            .developed
            .lock()
            .map_err(|_| "a poisoned cache")?
            .get(&key)
        {
            return Ok(done.clone());
        }
        let entry = service
            .apply("set-raw", json!({"temperature": temperature, "tint": tint}))
            .map_err(|error| format!("set-raw {temperature} K, tint {tint}: {error}"))?;
        let (image, recipe) = service
            .develop(&entry)
            .map_err(|error| format!("developing {temperature} K, tint {tint}: {error}"))?;
        let done = Arc::new(Developed {
            image,
            base: layers_of(&recipe, |_| false),
        });
        self.developed
            .lock()
            .map_err(|_| "a poisoned cache")?
            .insert(key, done.clone());
        Ok(done)
    }

    /// Whether the round's copy is still the photograph the manifest recorded.
    pub fn intact(&self) -> Result<bool> {
        Ok(hash(&self.path)? == self.sha256)
    }
}
