//! A captured frame: one state record from the run's result, the capture beside it, decoded once
//! and only when a check first reads it, and the accessors the scenarios read their state through.
use crate::*;
use image::RgbImage;
use std::{borrow::Borrow, cell::OnceCell, ops::Deref};

/// The run's own event log, one JSON object per line.
pub fn events(path: &Path) -> Result<Vec<Value>> {
    fs::read_to_string(path)?
        .lines()
        .map(|l| Ok(serde_json::from_str(l)?))
        .collect()
}

/// The run's own result and log, with the lifecycle, run identity and frame count checked. `frames`
/// is how many captures the scenario must have produced.
pub fn preamble(evidence: &Path, frames: usize) -> Result<(Value, Vec<Value>)> {
    let app = read_json(&evidence.join("result.json"))?;
    let events = events(&evidence.join("events.jsonl"))?;
    ensure(
        events.first().is_some_and(|e| e["event"] == "startup")
            && events.last().is_some_and(|e| e["event"] == "shutdown"),
        "Missing lifecycle",
    )?;
    ensure(app["status"] == "captured", "Unsuccessful app result")?;
    ensure(
        app["run_id"].as_str().is_some_and(|s| !s.is_empty())
            && events.iter().all(|e| e["run_id"] == app["run_id"]),
        "Wrong log run identity",
    )?;
    ensure(
        app["frames"]
            .as_array()
            .is_some_and(|captured| captured.len() == frames),
        "Missing/stale frames",
    )?;
    Ok((app, events))
}

/// A frame's provenance: whose run it belongs to, that it names a backend, that it came from a
/// renderer readback, and that the state file written beside it says the same thing. Returns the
/// capture's path.
pub fn identity(evidence: &Path, app: &Value, frame: &Value) -> Result<PathBuf> {
    let state = &frame["state"];
    ensure(state["run_id"] == app["run_id"], "Wrong frame run identity")?;
    ensure(
        ["backend", "adapter"]
            .iter()
            .all(|k| state["backend"][k].as_str().is_some_and(|s| !s.is_empty())),
        "Missing backend",
    )?;
    ensure(
        frame["capture_provenance"] == "window-renderer-readback",
        "Wrong capture provenance",
    )?;
    let name = Path::new(frame["file"].as_str().ok_or("Missing capture filename")?);
    ensure(
        name.components()
            .all(|c| matches!(c, std::path::Component::Normal(_))),
        "Unsafe capture path",
    )?;
    let number = name
        .file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(|stem| stem.strip_prefix("frame-"))
        .ok_or("Unexpected capture filename")?;
    ensure(
        &read_json(&evidence.join(format!("state-{number}.json")))? == frame,
        "The state file beside the frame disagrees with the run result",
    )?;
    Ok(evidence.join(name))
}

/// The physical x range of the photo surface a frame records, or `None` when it records none.
pub fn columns(frame: &Value) -> Result<Option<[u32; 2]>> {
    match frame.get("surface_columns") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let pair: [u32; 2] = serde_json::from_value(value.clone())?;
            Ok(Some(pair))
        }
    }
}

/// One captured frame: its record from the run's result, which it dereferences to, and its capture,
/// decoded the first time a check reads it and kept for every read after. A frame read for its
/// state before its provenance is checked has no capture to read.
pub struct Frame {
    record: Value,
    path: Option<PathBuf>,
    image: OnceCell<RgbImage>,
}

impl Deref for Frame {
    type Target = Value;
    fn deref(&self) -> &Value {
        &self.record
    }
}

impl Borrow<Value> for Frame {
    fn borrow(&self) -> &Value {
        &self.record
    }
}

impl Frame {
    fn new(record: &Value, path: Option<PathBuf>) -> Self {
        Self {
            record: record.clone(),
            path,
            image: OnceCell::new(),
        }
    }

    /// One frame of a run, its provenance checked by [`identity`].
    pub fn identified(evidence: &Path, app: &Value, record: &Value) -> Result<Self> {
        Ok(Self::new(record, Some(identity(evidence, app, record)?)))
    }

    /// A frame read for its recorded state alone, without checking its provenance.
    #[cfg(test)]
    pub fn state_only(record: &Value) -> Self {
        Self::new(record, None)
    }

    /// A frame over a capture a test made itself, with no run to check its provenance against.
    #[cfg(test)]
    pub fn unchecked(record: Value, path: &Path) -> Self {
        Self::new(&record, Some(path.into()))
    }

    /// The capture file, once the frame's provenance has been checked.
    pub fn path(&self) -> Result<&Path> {
        self.path
            .as_deref()
            .ok_or_else(|| "The frame's capture has not been identified".into())
    }

    /// The capture as 8-bit RGB, decoded on the first call.
    pub fn image(&self) -> Result<&RgbImage> {
        if let Some(image) = self.image.get() {
            return Ok(image);
        }
        let decoded = image::open(self.path()?)?.to_rgb8();
        Ok(self.image.get_or_init(|| decoded))
    }

    pub fn state(&self) -> &Value {
        &self.record["state"]
    }

    /// The photo surface's physical x range, as [`columns`] reads it.
    pub fn columns(&self) -> Result<Option<[u32; 2]>> {
        columns(&self.record)
    }

    /// The committed revision the frame shows.
    pub fn revision(&self) -> Result<u64> {
        self.state()["stack"]["revision"]
            .as_u64()
            .ok_or_else(|| "Frame records no revision".into())
    }

    /// The current history entry.
    pub fn entry(&self) -> Result<&str> {
        self.state()["stack"]["entry"]
            .as_str()
            .ok_or_else(|| "Frame records no current entry".into())
    }

    /// The current entry's history label.
    pub fn label(&self) -> Result<&str> {
        self.state()["stack"]["label"]
            .as_str()
            .ok_or_else(|| "Frame records no history label".into())
    }

    /// The status bar's text.
    pub fn status(&self) -> Result<&str> {
        self.state()["status"]
            .as_str()
            .ok_or_else(|| "Frame records no status".into())
    }

    /// The notices on the canvas, as text; none when the frame records none.
    pub fn notices(&self) -> Vec<String> {
        self.state()["notices"]
            .as_array()
            .map(|notices| {
                notices
                    .iter()
                    .filter_map(|notice| notice.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The slider draft the frame's session reported, or `Null` when the client held none.
    pub fn draft(&self) -> &Value {
        &self.state()["draft"]
    }

    pub fn expect_no_draft(&self, what: &str) -> Result {
        ensure(
            self.draft() == &Value::Null,
            format!("{what}: a draft is still open: {}", self.draft()),
        )
    }

    /// What one generated field of `action` showed when the frame was captured.
    pub fn field(&self, action: &str, name: &str) -> Result<&str> {
        self.state()["controls"][format!("{action}.{name}")]
            .as_str()
            .ok_or_else(|| format!("Frame records no {name} field").into())
    }

    pub fn section_expanded(&self, module: &str) -> bool {
        self.state()["expanded"][module] == json!(true)
    }

    /// The expanded state of each of `modules`' sections, for the correlation a recorded frame
    /// carries alongside its revision, entry and draft.
    pub fn expanded_sections(&self, modules: &[&str]) -> Value {
        Value::Object(
            modules
                .iter()
                .map(|module| ((*module).to_owned(), json!(self.section_expanded(module))))
                .collect(),
        )
    }

    /// The committed stack's first layer of `effect`, or `None` when it holds none.
    pub fn layer(&self, effect: &str) -> Option<&Value> {
        self.state()["stack"]["layers"]
            .as_array()?
            .iter()
            .find(|layer| layer["effect"] == json!(effect))
    }

    /// That layer's stored payload.
    pub fn payload(&self, effect: &str) -> Option<&Value> {
        self.layer(effect).map(|layer| &layer["payload"])
    }

    /// That layer's identity, so evidence can prove an edit updated it in place.
    pub fn layer_id(&self, effect: &str) -> Option<&str> {
        self.layer(effect).and_then(|layer| layer["id"].as_str())
    }

    /// The masks the Masks panel lists.
    pub fn masks(&self) -> Result<&Vec<Value>> {
        self.state()["masks"]["masks"]
            .as_array()
            .ok_or_else(|| "Frame records no mask list".into())
    }

    /// The one mask a scenario draws, or an error naming what was listed.
    pub fn only_mask(&self) -> Result<&Value> {
        let masks = self.masks()?;
        ensure(
            masks.len() == 1,
            format!("Expected exactly one mask, found {}", json!(masks)),
        )?;
        Ok(&masks[0])
    }

    /// Every component of the open mask, as the panel derived them.
    pub fn components(&self) -> Result<&Vec<Value>> {
        self.state()["masks"]["components"]
            .as_array()
            .ok_or_else(|| "Frame records no component list".into())
    }

    pub fn component(&self, index: usize) -> Result<&Value> {
        self.components()?
            .get(index)
            .ok_or_else(|| format!("The mask holds no component {index}").into())
    }

    /// The modes and kinds of the open mask's components, in list order.
    pub fn kinds(&self) -> Result<Vec<String>> {
        Ok(self
            .components()?
            .iter()
            .map(|component| {
                format!(
                    "{} {}",
                    component["mode"].as_str().unwrap_or_default(),
                    component["kind"].as_str().unwrap_or_default()
                )
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(record: Value) -> Frame {
        Frame::state_only(&record)
    }

    #[test]
    fn accessors_read_the_state_and_name_what_is_missing() {
        let recorded = frame(json!({"state":{
            "stack":{"revision":3,"entry":"e","label":"Exposure +1.00 EV","layers":[
                {"effect":"a","id":"one","payload":{"x":1.0}},
                {"effect":"b","id":"two","payload":{}}
            ]},
            "expanded":{"m":true,"n":false},
            "controls":{"set-basic.exposure":"1.00"},
            "notices":["Changed elsewhere", 3],
            "masks":{"masks":[{"id":"m1"}],"components":[{"mode":"add","kind":"linear"}]}
        }}));
        assert_eq!(recorded.revision().unwrap(), 3);
        assert_eq!(recorded.entry().unwrap(), "e");
        assert_eq!(recorded.label().unwrap(), "Exposure +1.00 EV");
        assert_eq!(recorded.field("set-basic", "exposure").unwrap(), "1.00");
        assert!(
            recorded
                .field("set-basic", "tint")
                .unwrap_err()
                .to_string()
                .contains("no tint field")
        );
        assert_eq!(recorded.notices(), ["Changed elsewhere"]);
        assert!(recorded.expect_no_draft("Frame 1").is_ok());
        assert_eq!(
            recorded.expanded_sections(&["m", "n", "o"]),
            json!({"m":true,"n":false,"o":false})
        );
        assert_eq!(recorded.payload("a"), Some(&json!({"x":1.0})));
        assert_eq!(recorded.layer_id("b"), Some("two"));
        assert!(recorded.layer("c").is_none());
        assert_eq!(recorded.only_mask().unwrap()["id"], "m1");
        assert_eq!(recorded.kinds().unwrap(), ["add linear"]);
        assert!(recorded.component(1).is_err());

        let empty = frame(json!({"state":{"draft":{"fields":{}}}}));
        assert!(
            empty
                .revision()
                .unwrap_err()
                .to_string()
                .contains("revision")
        );
        assert!(empty.expect_no_draft("Frame 2").is_err());
        assert!(empty.masks().is_err() && empty.notices().is_empty());
    }

    #[test]
    fn the_capture_is_decoded_once_and_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("frame-1.png");
        image::RgbImage::from_pixel(4, 2, image::Rgb([10, 20, 30]))
            .save(&path)
            .unwrap();
        let captured = Frame::unchecked(json!({}), &path);
        assert_eq!(captured.image().unwrap().dimensions(), (4, 2));
        // Once decoded, the capture is read from memory: removing the file changes nothing.
        fs::remove_file(&path).unwrap();
        assert_eq!(captured.image().unwrap().get_pixel(3, 1).0, [10, 20, 30]);
        let missing = frame(json!({}));
        assert!(missing.image().is_err());
    }
}
