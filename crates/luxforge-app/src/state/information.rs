//! The image-information readout: original capture fields and the selected recipe's output size.
//! This borrows the per-photo metadata already read on open and performs no owner or pixel work.
use super::Inputs;
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Information {
    pub(crate) dimensions: Option<(u32, u32)>,
    pub(crate) rows: Vec<(&'static str, String)>,
}

fn number(value: f64) -> String {
    format!("{value:.6}")
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
}

fn shutter(seconds: f64) -> String {
    let reciprocal = 1.0 / seconds;
    if seconds < 1.0 && (reciprocal - reciprocal.round()).abs() < 0.001 {
        format!("1/{} s", reciprocal.round())
    } else {
        format!("{} s", number(seconds))
    }
}

fn identity(make: Option<&str>, model: Option<&str>) -> Option<String> {
    match (make, model) {
        (Some(make), Some(model)) => {
            // Camera makes can include a corporate suffix absent from their model's name.
            let maker = make.split_whitespace().next().unwrap_or(make);
            if model.to_lowercase().starts_with(&maker.to_lowercase()) {
                Some(model.to_owned())
            } else {
                Some(format!("{make} {model}"))
            }
        }
        (Some(value), None) | (None, Some(value)) => Some(value.to_owned()),
        (None, None) => None,
    }
}

pub(crate) fn derive(inputs: &Inputs<'_>) -> Option<Information> {
    if !inputs.session.workspace.information {
        return None;
    }
    let state = inputs.document.state.as_ref()?;
    let dimensions = match inputs.draft {
        Some(draft) => draft
            .output()
            .ok()
            .map(|output| (output.width, output.height)),
        None => inputs
            .document
            .recipe
            .as_ref()
            .and_then(|recipe| recipe.output_stage.map(|stage| (stage.width, stage.height))),
    };
    let resolution = |size: (u32, u32)| format!("{} × {} px", size.0, size.1);
    let mut rows = vec![(
        if inputs.draft.is_some() {
            "Crop"
        } else {
            "Resolution"
        },
        dimensions
            .map(resolution)
            .unwrap_or_else(|| "Unavailable".into()),
    )];
    let original = (state.asset.width, state.asset.height);
    if dimensions != Some(original) {
        rows.push(("Original", resolution(original)));
    }
    let empty = luxforge_core::CaptureInfo::default();
    let capture = inputs.document.capture.as_ref().unwrap_or(&empty);
    let mut add = |label, value: Option<String>| {
        rows.push((label, value.unwrap_or_else(|| "Unavailable".into())));
    };
    add(
        "Camera",
        identity(capture.make.as_deref(), capture.model.as_deref()),
    );
    add(
        "Lens",
        identity(capture.lens_make.as_deref(), capture.lens_model.as_deref()),
    );
    add(
        "Aperture",
        capture.aperture.map(|value| format!("f/{}", number(value))),
    );
    add("Shutter", capture.exposure_seconds.map(shutter));
    add("ISO", capture.iso.map(|value| value.to_string()));
    add(
        "Focal length",
        capture
            .focal_mm
            .map(|value| {
                let focal = format!("{} mm", number(value));
                match capture.focal_35mm {
                    Some(equivalent) if (value - f64::from(equivalent)).abs() > 0.01 => {
                        format!("{focal} · {equivalent} mm equiv.")
                    }
                    _ => focal,
                }
            })
            .or_else(|| capture.focal_35mm.map(|value| format!("{value} mm equiv."))),
    );
    if capture.captured_at.is_some() {
        add("Captured", capture.captured_at.clone());
    }
    Some(Information { dimensions, rows })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn information_formats_capture_values_without_inventing_identity_or_rounding_long_exposures() {
        assert_eq!(shutter(1.0 / 250.0), "1/250 s");
        assert_eq!(shutter(0.4), "0.4 s");
        assert_eq!(shutter(1.5), "1.5 s");
        assert_eq!(number(2.8), "2.8");
        assert_eq!(
            identity(Some("NIKON CORPORATION"), Some("NIKON Z 6")),
            Some("NIKON Z 6".into())
        );
        assert_eq!(
            identity(Some("FUJIFILM"), Some("X100VI")),
            Some("FUJIFILM X100VI".into())
        );
        assert_eq!(identity(None, None), None);
    }
}
