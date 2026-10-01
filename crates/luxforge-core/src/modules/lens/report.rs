//! A prefilled GitHub "new issue" page asking for a missing lens profile. The core builds it so a
//! person and an agent read the same report; opening it publishes nothing, since the person reviews
//! and submits it on GitHub. It names the camera, lens and optics only: never a file name or path,
//! a serial number, a location or any other personal metadata.
use super::{pinned, resolve::Reason};
use crate::SourceOptics;
use serde_json::{Value, json};

/// The project's issue tracker; the one place the report's destination is named.
pub(crate) const NEW_ISSUE_URL: &str = "https://github.com/Willyham/luxforge/issues/new";
/// The longest report URL. Browsers and GitHub accept longer, but a prefilled page this size
/// always opens.
pub(crate) const MAX_REPORT_URL: usize = 6000;
pub(crate) const REPORT_LABEL: &str = "Report missing lens";

/// What the report says about the photo.
pub(crate) struct Facts<'a> {
    pub camera_name: &'a str,
    pub fixed: bool,
    pub optics: &'a SourceOptics,
    pub focal_mm: Option<f64>,
    pub crop_factor: Option<f64>,
    pub reasons: &'a [Reason],
}

/// RFC 3986 percent-encoding of a query value: unreserved characters stay, every other UTF-8 byte
/// is `%XX`.
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(byte));
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// One metadata string as a single table cell: control characters dropped, pipes escaped and at
/// most `limit` characters, so a long or hostile EXIF string cannot reshape the report.
fn cell(value: Option<&str>, limit: usize) -> String {
    let cleaned: String = value
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .take(limit)
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        "—".into()
    } else {
        cleaned.replace('|', "\\|")
    }
}

fn status(value: luxforge_raw::OpticalStatus) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// RAW, DNG or JPEG, from the interpretation marker the ledger reports.
fn source_kind(interpretation: &str) -> &'static str {
    if interpretation == "jpeg" {
        "JPEG"
    } else if interpretation.starts_with("raw-mosaic:") {
        "RAW"
    } else {
        "DNG"
    }
}

fn build(facts: &Facts<'_>, limit: usize) -> (String, String) {
    let identity = &facts.optics.identity;
    let ledger = &facts.optics.ledger;
    let lens = if facts.fixed {
        "built-in lens".to_owned()
    } else {
        identity
            .lens_model
            .as_deref()
            .map_or_else(|| "unnamed lens".to_owned(), |l| cell(Some(l), limit))
    };
    let title = format!(
        "Lens profile request: {} · {lens}",
        cell(Some(facts.camera_name), limit)
    );
    let focal = match (facts.focal_mm, identity.focal_35mm) {
        (Some(f), Some(f35)) => format!("{f} mm (35 mm equivalent {f35} mm)"),
        (Some(f), None) => format!("{f} mm"),
        (None, Some(f35)) => format!("missing (35 mm equivalent {f35} mm)"),
        (None, None) => "missing".into(),
    };
    let reasons = facts
        .reasons
        .iter()
        .map(|reason| reason.label())
        .collect::<Vec<_>>()
        .join(", ");
    let rows = [
        ("Camera", cell(Some(facts.camera_name), limit)),
        (
            "EXIF make / model",
            format!(
                "{} / {}",
                cell(identity.make.as_deref(), limit),
                cell(identity.model.as_deref(), limit)
            ),
        ),
        (
            "EXIF lens make / model",
            format!(
                "{} / {}",
                cell(identity.lens_make.as_deref(), limit),
                cell(identity.lens_model.as_deref(), limit)
            ),
        ),
        ("Focal length", focal),
        (
            "Crop factor",
            facts
                .crop_factor
                .map_or_else(|| "unknown".into(), |c| format!("{c} (database)")),
        ),
        (
            "Source",
            format!(
                "{} ({})",
                source_kind(&ledger.interpretation),
                cell(Some(&ledger.interpretation), limit)
            ),
        ),
        (
            "In-camera distortion correction",
            format!(
                "{} ({})",
                status(ledger.distortion.status),
                cell(Some(&ledger.distortion.provenance), limit)
            ),
        ),
        (
            "Reasons",
            if reasons.is_empty() {
                "—".into()
            } else {
                reasons
            },
        ),
        (
            "Lens database",
            format!("{} ({})", pinned::RELEASE, pinned::COMMIT),
        ),
        ("Luxforge", env!("CARGO_PKG_VERSION").to_owned()),
    ];
    let mut body = String::from(
        "Luxforge found no compatible lens profile for this photo. Please add support for this \
         camera and lens.\n\n| Field | Value |\n| --- | --- |\n",
    );
    for (field, value) in rows {
        body.push_str(&format!("| {field} | {value} |\n"));
    }
    body.push_str(
        "\nThis report names the camera, lens and optics only: no file name, path, serial number \
         or location.\n",
    );
    (title, body)
}

/// The report as `{label, url}`, the query-choice vocabulary's report link.
pub(crate) fn report(facts: &Facts<'_>) -> Value {
    // Ordinary metadata fits with 64 characters a field; a long non-ASCII identity is cut further
    // until the whole URL fits.
    let url = [64, 24, 8]
        .into_iter()
        .map(|limit| {
            let (title, body) = build(facts, limit);
            format!(
                "{NEW_ISSUE_URL}?title={}&body={}",
                encode(&title),
                encode(&body)
            )
        })
        .find(|url| url.len() <= MAX_REPORT_URL)
        .unwrap_or_else(|| NEW_ISSUE_URL.to_owned());
    json!({"label": REPORT_LABEL, "url": url})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OpticalIdentity;

    fn decode(text: &str) -> String {
        let bytes = text.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' {
                out.push(u8::from_str_radix(&text[i + 1..i + 3], 16).unwrap());
                i += 3;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        String::from_utf8(out).unwrap()
    }

    fn optics(make: &str, model: &str, lens: Option<&str>) -> SourceOptics {
        SourceOptics {
            identity: OpticalIdentity {
                make: Some(make.into()),
                model: Some(model.into()),
                lens_make: None,
                lens_model: lens.map(str::to_owned),
                focal_mm: Some(23.0),
                focal_35mm: Some(35),
            },
            ledger: SourceOptics::jpeg_ledger(),
        }
    }

    fn query(url: &str) -> (String, String) {
        let rest = url.strip_prefix(NEW_ISSUE_URL).unwrap();
        let rest = rest.strip_prefix("?title=").unwrap();
        let (title, body) = rest.split_once("&body=").unwrap();
        (decode(title), decode(body))
    }

    #[test]
    fn lens_report_prefills_an_issue_with_identity_optics_and_database_only() {
        let optics = optics("FUJIFILM", "X100VI", None);
        let report = report(&Facts {
            camera_name: "FUJIFILM X100VI",
            fixed: false,
            optics: &optics,
            focal_mm: Some(23.0),
            crop_factor: None,
            reasons: &[Reason::CameraNotInDatabase],
        });
        assert_eq!(report["label"], REPORT_LABEL);
        let url = report["url"].as_str().unwrap();
        assert!(url.starts_with("https://github.com/Willyham/luxforge/issues/new?title="));
        assert!(url.len() <= MAX_REPORT_URL);
        assert!(
            url.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.~%?=&:/".contains(&b)),
            "{url}"
        );
        let (title, body) = query(url);
        assert_eq!(
            title,
            "Lens profile request: FUJIFILM X100VI · unnamed lens"
        );
        for expected in [
            "| EXIF make / model | FUJIFILM / X100VI |",
            "| Focal length | 23 mm (35 mm equivalent 35 mm) |",
            "| Crop factor | unknown |",
            "| Source | JPEG (jpeg) |",
            "| In-camera distortion correction | unknown (jpeg-exif-none) |",
            "| Reasons | camera-not-in-database |",
            "lensfun-0.3.4 (101c745e847a5de4a1e569a94368ce2027198598)",
            "| Luxforge | ",
        ] {
            assert!(body.contains(expected), "{expected}\n{body}");
        }
    }

    #[test]
    fn lens_report_stays_bounded_and_one_cell_per_hostile_field() {
        let hostile = "𝔑|\n".repeat(400);
        let optics = optics(&hostile, &hostile, Some(&hostile));
        let report = report(&Facts {
            camera_name: &hostile,
            fixed: false,
            optics: &optics,
            focal_mm: None,
            crop_factor: Some(1.5),
            reasons: &[Reason::LensNotInDatabase],
        });
        let url = report["url"].as_str().unwrap();
        assert!(url.len() <= MAX_REPORT_URL, "{}", url.len());
        let (_, body) = query(url);
        // Every table row is still one line with two unescaped separators either side.
        for line in body.lines().filter(|l| l.starts_with("| ")) {
            assert_eq!(line.replace("\\|", "").matches('|').count(), 3, "{line}");
        }
    }
}
