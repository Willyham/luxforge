//! The bodies the host builds for a provider adapter from a task's declared data class. A module
//! never supplies a body: the catalog owner binds the asset's current entry when the task is
//! requested, the host samples it on the worker and builds the body here, and sends exactly it, so
//! what a consent notice discloses is what leaves the machine. See
//! `docs/design/module-capabilities.md#provider-adapters`.
use super::descriptor::{DataClass, SAMPLE_GRID_SIDE};
#[cfg(test)]
use crate::ErrorKind;
use crate::{Error, editor::SamplePlan};
use std::{
    fmt::Write as _,
    sync::{Mutex, PoisonError},
};

/// What precedes the samples of a `sample-grid-8` body.
const SAMPLE_GRID_PREFIX: &str =
    r#"{"data_class":"sample-grid-8","width":8,"height":8,"samples":["#;
/// The samples in one grid.
pub const SAMPLE_GRID_SAMPLES: usize = (SAMPLE_GRID_SIDE * SAMPLE_GRID_SIDE) as usize;
/// One sample as written: `[rrr,ggg,bbb]`, each channel right-aligned in three characters.
const SAMPLE_BYTES: usize = 13;
/// The exact length of every `sample-grid-8` body: the prefix, the samples and the commas between
/// them, and the closing `]}`.
pub const SAMPLE_GRID_BYTES: usize =
    SAMPLE_GRID_PREFIX.len() + SAMPLE_GRID_SAMPLES * SAMPLE_BYTES + (SAMPLE_GRID_SAMPLES - 1) + 2;

/// The `sample-grid-8` body of 64 point samples, row by row from the top-left. Every channel is
/// padded to three characters with spaces, which JSON allows between tokens, so the body's length
/// is [`SAMPLE_GRID_BYTES`] whatever the samples are and the consent notice can state it exactly
/// before any pixel is read.
pub fn sample_grid_body(samples: &[[u8; 3]]) -> Result<Vec<u8>, Error> {
    if samples.len() != SAMPLE_GRID_SAMPLES {
        return Err(Error::internal(format!(
            "a sample grid holds {SAMPLE_GRID_SAMPLES} samples, not {}",
            samples.len()
        )));
    }
    let mut body = String::with_capacity(SAMPLE_GRID_BYTES);
    body.push_str(SAMPLE_GRID_PREFIX);
    for (index, [red, green, blue]) in samples.iter().enumerate() {
        if index > 0 {
            body.push(',');
        }
        let _ = write!(body, "[{red:>3},{green:>3},{blue:>3}]");
    }
    body.push_str("]}");
    debug_assert_eq!(body.len(), SAMPLE_GRID_BYTES);
    Ok(body.into_bytes())
}

/// The data one granted remote request of a task discloses, bound by the catalog owner when the
/// task was requested and turned into its body on the worker the first time the task sends. For
/// `sample-grid-8` that is 64 point samples of the entry the plan bound, which a spatial layer makes
/// too costly for the owner; the body is built once per job.
pub(crate) struct DisclosedData {
    class: DataClass,
    plan: SamplePlan,
    body: Mutex<Option<Vec<u8>>>,
}

impl DisclosedData {
    pub(crate) fn new(class: DataClass, plan: SamplePlan) -> Self {
        Self {
            class,
            plan,
            body: Mutex::new(None),
        }
    }

    pub(crate) fn class(&self) -> DataClass {
        self.class
    }

    /// The body, built the first time it is asked for. `checkpoint` is asked before each sample,
    /// so a cancelled task stops between them.
    pub(crate) fn body(
        &self,
        checkpoint: &dyn Fn() -> Result<(), Error>,
    ) -> Result<Vec<u8>, Error> {
        let mut body = self.body.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(bytes) = body.as_ref() {
            return Ok(bytes.clone());
        }
        let bytes = match self.class {
            DataClass::SampleGrid8 => {
                let samples: Vec<[u8; 3]> = self
                    .plan
                    .grid(SAMPLE_GRID_SIDE, checkpoint)?
                    .into_iter()
                    .map(|[red, green, blue, _]| [red, green, blue])
                    .collect();
                sample_grid_body(&samples)?
            }
        };
        *body = Some(bytes.clone());
        Ok(bytes)
    }
}

/// The data is a photo's: its `Debug` names the class only.
impl std::fmt::Debug for DisclosedData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DisclosedData")
            .field("class", &self.class.name())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn a_sample_grid_body_is_json_of_the_disclosed_length_whatever_it_holds() {
        for samples in [
            vec![[0, 0, 0]; SAMPLE_GRID_SAMPLES],
            vec![[255, 255, 255]; SAMPLE_GRID_SAMPLES],
            (0..SAMPLE_GRID_SAMPLES)
                .map(|index| [index as u8, (index * 3) as u8, 255 - index as u8])
                .collect(),
        ] {
            let body = sample_grid_body(&samples).unwrap();
            assert_eq!(body.len(), SAMPLE_GRID_BYTES);
            assert_eq!(
                body.len() as u64,
                DataClass::SampleGrid8.request_bytes(),
                "what the consent notice discloses is what is sent"
            );
            let parsed: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(parsed["data_class"], json!("sample-grid-8"));
            assert_eq!(parsed["width"], json!(8));
            assert_eq!(parsed["height"], json!(8));
            assert_eq!(parsed["samples"], json!(samples));
        }
        assert_eq!(SAMPLE_GRID_BYTES, 959);
        assert_eq!(DataClass::SampleGrid8.content_type(), "application/json");
        let body = sample_grid_body(&[[5, 12, 255]; SAMPLE_GRID_SAMPLES]).unwrap();
        assert!(String::from_utf8(body).unwrap().contains("[  5, 12,255]"));
        assert_eq!(
            sample_grid_body(&[[0, 0, 0]; 63]).unwrap_err().kind,
            ErrorKind::Internal
        );
    }
}
