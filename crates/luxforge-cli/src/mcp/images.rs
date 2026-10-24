//! Pictures attached to results ([results](../../../../docs/design/mcp.md#results)), found
//! through the registry's own markers and never by method: `images` lists the dotted fields of a
//! method's answer that hold the path of a JPEG the owner wrote for display, and `job_images`, on
//! `job.read` and `job.wait`, names the answer's job-kind field and, per job kind, such fields of
//! its result. An export's output is never marked, so a full-resolution file is never attached.
use base64::Engine;
use luxforge_core::ApiFailure;
use serde_json::{Value, json};
use std::io::Read;

/// The largest file attached. The largest display tier is 2048 px, a few hundred kilobytes.
pub const IMAGE_LIMIT: u64 = 8 * 1024 * 1024;
/// The MIME type of every attached picture: the owner writes JPEGs for display.
pub const MIME_TYPE: &str = "image/jpeg";

/// The warning when a marked picture is larger than [`IMAGE_LIMIT`].
pub const IMAGE_TOO_LARGE: &str = "image-too-large";
/// The warning when a marked picture cannot be read.
pub const IMAGE_UNREADABLE: &str = "image-unreadable";

/// The value at a dotted path, such as `preview.path`.
fn at<'a>(value: &'a Value, dotted: &str) -> Option<&'a Value> {
    dotted
        .split('.')
        .try_fold(value, |value, field| value.get(field))
}

/// The marked fields of `answer` that hold a path, with their paths, in marker order and each path
/// once: `entry`'s `images`, then its `job_images` fields for the kind the answer names.
pub fn marked(entry: &Value, answer: &Value) -> Vec<(String, String)> {
    let mut fields: Vec<String> = entry["images"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    let job_images = &entry["job_images"];
    if let Some(kind) = job_images["kind"]
        .as_str()
        .and_then(|kind| at(answer, kind))
        .and_then(Value::as_str)
    {
        fields.extend(
            job_images["fields"][kind]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned),
        );
    }
    let mut found: Vec<(String, String)> = Vec::new();
    for field in fields {
        if let Some(path) = at(answer, &field).and_then(Value::as_str)
            && !found.iter().any(|(_, seen)| seen == path)
        {
            found.push((field, path.to_owned()));
        }
    }
    found
}

/// The picture at `path`, base64-encoded, or the warning that says why it is not attached.
pub fn read(field: &str, path: &str) -> Result<String, ApiFailure> {
    let warning = |code: &str, message: String, data: Value| ApiFailure {
        code: code.into(),
        message,
        job_id: None,
        data: Some(data),
    };
    let unreadable = |error: std::io::Error| {
        warning(
            IMAGE_UNREADABLE,
            format!("the picture {field} names cannot be read, so it is not attached: {error}"),
            json!({"field": field, "path": path}),
        )
    };
    let too_large = |bytes: u64| {
        warning(
            IMAGE_TOO_LARGE,
            format!(
                "the picture {field} names is {bytes} bytes, over the {IMAGE_LIMIT}-byte limit, so \
                 it is not attached; its path is in the result"
            ),
            json!({"field": field, "path": path, "bytes": bytes, "limit": IMAGE_LIMIT}),
        )
    };
    let file = std::fs::File::open(path).map_err(unreadable)?;
    let length = file.metadata().map_err(unreadable)?.len();
    if length > IMAGE_LIMIT {
        return Err(too_large(length));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(length).unwrap_or(0));
    file.take(IMAGE_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(unreadable)?;
    if bytes.len() as u64 > IMAGE_LIMIT {
        return Err(too_large(bytes.len() as u64));
    }
    Ok(base64::engine::general_purpose::STANDARD.encode(&bytes))
}

#[cfg(test)]
mod tests;
