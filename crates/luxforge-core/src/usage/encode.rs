//! One statistics batch on the wire (`docs/design/usage-statistics-wire.md`): the strict encoder the
//! worker serializes a detached window with, and an independent strict decoder that validates a
//! body against the same format, for tests and the end-to-end check.
//!
//! The encoder writes compact JSON into one [`BodyBuffer`] whose 64 KiB capacity is reserved once
//! and never grows. If the whole window would not fit, it omits `process.sample` events first
//! (newest first), then the newest other events, counting each in `dropped`: never a partial
//! document and never a second request. Every string it writes is a static registry value, the
//! validated release, the two random IDs, the session start or a keyed image ID.
use super::{
    device_profile::{DeviceProfile, enum_of},
    event::{
        AddedCount, DurationMs, Event, EventKind, MemoryMib, OperationEnd, Percent, ProcessSample,
    },
    image_id::ImageId,
    registry::{
        EventName, Feature, Idle, MAX_BODY_BYTES, MAX_DEVICE_BYTES, MAX_EVENTS, MemoryKind,
        Operation, Outcome, Presented, Renderer, SCHEMA, Tool, Trigger, Workspace,
    },
};
use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use std::{
    fmt,
    io::{self, Write},
};
use uuid::{Uuid, Variant};

/// A canonical random UUIDv4 identifying an installation or a session.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReportId(Uuid);

impl ReportId {
    /// A fresh cryptographically random ID.
    pub fn random() -> Self {
        Self(Uuid::new_v4())
    }

    /// `uuid` if it is version 4 with the RFC 4122 variant.
    pub fn from_uuid(uuid: Uuid) -> Option<Self> {
        (uuid.get_version_num() == 4 && uuid.get_variant() == Variant::RFC4122)
            .then_some(Self(uuid))
    }

    /// Exactly the canonical lowercase hyphenated form of a UUIDv4; nothing else.
    pub fn parse(text: &str) -> Option<Self> {
        let uuid = Uuid::try_parse(text).ok()?;
        let id = Self::from_uuid(uuid)?;
        (id.to_wire() == text.as_bytes()).then_some(id)
    }

    pub fn uuid(&self) -> Uuid {
        self.0
    }

    /// The canonical form, on the stack.
    pub fn to_wire(&self) -> [u8; 36] {
        let mut out = [0; 36];
        self.0.hyphenated().encode_lower(&mut out);
        out
    }
}

impl fmt::Debug for ReportId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ReportId({})", self.0.hyphenated())
    }
}

/// The reported release: the workspace version plus 12 hexadecimal characters of the clean source
/// revision (`0.0.0+9d4138ca1f2e`), held inline.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Release {
    bytes: [u8; Self::MAX],
    len: u8,
}

impl Release {
    /// Three components of at most nine digits, two dots, `+` and 12 hex digits.
    const MAX: usize = 9 * 3 + 2 + 1 + 12;

    /// `text` if it matches
    /// `^(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})\+[0-9a-f]{12}$`.
    pub fn parse(text: &str) -> Option<Self> {
        let (version, revision) = text.split_once('+')?;
        if revision.len() != 12
            || !revision
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        {
            return None;
        }
        let mut parts = version.split('.');
        for _ in 0..3 {
            let part = parts.next()?.as_bytes();
            let valid = match part {
                [b'0'] => true,
                [b'1'..=b'9', rest @ ..] => rest.len() <= 8 && rest.iter().all(u8::is_ascii_digit),
                _ => false,
            };
            if !valid {
                return None;
            }
        }
        if parts.next().is_some() {
            return None;
        }
        let mut bytes = [0; Self::MAX];
        bytes[..text.len()].copy_from_slice(text.as_bytes());
        Some(Self {
            bytes,
            len: text.len() as u8,
        })
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..usize::from(self.len)]).expect("ASCII")
    }
}

impl fmt::Debug for Release {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The UTC wall-clock second a session began, in years 2020 to 2200.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionStart(u64);

impl SessionStart {
    /// 2020-01-01T00:00:00Z.
    const FIRST: u64 = 1_577_836_800;
    /// 2200-12-31T23:59:59Z.
    const LAST: u64 = 7_289_654_399;

    /// Seconds since the Unix epoch, if within years 2020 to 2200.
    pub fn from_unix_seconds(seconds: u64) -> Option<Self> {
        (Self::FIRST..=Self::LAST)
            .contains(&seconds)
            .then_some(Self(seconds))
    }

    /// Now, to the second; `None` when the system clock is outside the wire's years.
    pub fn now() -> Option<Self> {
        let since = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?;
        Self::from_unix_seconds(since.as_secs())
    }

    pub fn unix_seconds(self) -> u64 {
        self.0
    }

    /// `YYYY-MM-DDTHH:MM:SSZ`, on the stack.
    pub fn to_wire(self) -> [u8; 20] {
        let days = (self.0 / 86_400) as i64;
        let second_of_day = self.0 % 86_400;
        let (year, month, day) = civil_from_days(days);
        let mut out = *b"0000-00-00T00:00:00Z";
        let mut put = |at: usize, value: u64, width: usize| {
            let mut value = value;
            for index in (0..width).rev() {
                out[at + index] = b'0' + (value % 10) as u8;
                value /= 10;
            }
        };
        put(0, year as u64, 4);
        put(5, u64::from(month), 2);
        put(8, u64::from(day), 2);
        put(11, second_of_day / 3600, 2);
        put(14, second_of_day / 60 % 60, 2);
        put(17, second_of_day % 60, 2);
        out
    }

    /// Exactly `YYYY-MM-DDTHH:MM:SSZ`, a valid calendar time in years 2020 to 2200.
    pub fn parse(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        if bytes.len() != 20 {
            return None;
        }
        for (index, byte) in bytes.iter().enumerate() {
            let ok = match index {
                4 | 7 => *byte == b'-',
                10 => *byte == b'T',
                13 | 16 => *byte == b':',
                19 => *byte == b'Z',
                _ => byte.is_ascii_digit(),
            };
            if !ok {
                return None;
            }
        }
        let number = |from: usize, to: usize| -> u64 {
            bytes[from..to]
                .iter()
                .fold(0, |acc, b| acc * 10 + u64::from(b - b'0'))
        };
        let (year, month, day) = (number(0, 4), number(5, 7), number(8, 10));
        let (hour, minute, second) = (number(11, 13), number(14, 16), number(17, 19));
        if !(1..=12).contains(&month) || hour > 23 || minute > 59 || second > 59 {
            return None;
        }
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let days_in_month = match month {
            2 if leap => 29,
            2 => 28,
            4 | 6 | 9 | 11 => 30,
            _ => 31,
        };
        if !(1..=days_in_month).contains(&day) {
            return None;
        }
        let days = days_from_civil(year as i64, month as u32, day as u32);
        let seconds = u64::try_from(days).ok()? * 86_400 + hour * 3600 + minute * 60 + second;
        Self::from_unix_seconds(seconds)
    }
}

/// Howard Hinnant's `civil_from_days`: the proleptic Gregorian date of a day count from the epoch.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Howard Hinnant's `days_from_civil`.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400);
    let month = i64::from(month);
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// What one batch's envelope says beyond its events.
#[derive(Clone, Copy, Debug)]
pub struct Envelope<'a> {
    pub release: &'a Release,
    pub installation_id: ReportId,
    pub session_id: ReportId,
    pub session_start: SessionStart,
    pub batch_seq: u32,
    pub device: &'a DeviceProfile,
}

/// The one serialized body: 64 KiB reserved once, never grown.
#[derive(Debug)]
pub struct BodyBuffer(Vec<u8>);

impl Default for BodyBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl BodyBuffer {
    pub fn new() -> Self {
        Self(Vec::with_capacity(MAX_BODY_BYTES))
    }

    /// The last encoded body.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Forget the body, keeping the reserved capacity.
    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// The allocation capacity it owns.
    pub fn storage_bytes(&self) -> usize {
        self.0.capacity()
    }

    /// Exchange the body's storage with `other`, so the sender hands the encoded body to the
    /// transport's request and takes the reserved storage back afterwards, without a second
    /// 64 KiB copy.
    pub(crate) fn swap(&mut self, other: &mut Vec<u8>) {
        std::mem::swap(&mut self.0, other);
    }
}

/// What [`encode_batch`] wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Encoded {
    /// Events in the body.
    pub sent: usize,
    /// Events of the window the body limits left out.
    pub omitted: usize,
    /// The body's `dropped`: the window's dropped events plus those omitted.
    pub dropped: u32,
}

/// Why a window could not be encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncodeError {
    /// No event to send; an empty batch is never sent.
    Empty,
    /// The events are not in strictly increasing `seq` with non-decreasing `t`.
    Order,
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Empty => "no event to send",
            Self::Order => "events are out of order",
        })
    }
}

impl std::error::Error for EncodeError {}

/// Counts written bytes, for sizes without a buffer.
#[derive(Debug, Default)]
pub(super) struct Counter(pub usize);

impl Write for Counter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0 += buf.len();
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Serialize one batch of `events` (a window in recording order), whose window dropped
/// `window_dropped` events, into `body`. See the module's description for the limits.
pub fn encode_batch(
    envelope: &Envelope<'_>,
    events: &[Event],
    window_dropped: u32,
    body: &mut BodyBuffer,
) -> Result<Encoded, EncodeError> {
    if events
        .windows(2)
        .any(|pair| pair[1].seq <= pair[0].seq || pair[1].t < pair[0].t)
    {
        return Err(EncodeError::Order);
    }
    let considered = events.len().min(MAX_EVENTS);
    let mut omitted = events.len() - considered;
    // Each event costs its length plus a separating comma; the head is sized with the widest
    // possible `dropped`, so the final body can only be shorter than this budget.
    let mut head = Counter::default();
    write_head(&mut head, envelope, u32::MAX).expect("counting cannot fail");
    let mut total = head.0 + TAIL.len() - 1;
    let mut keep = [true; MAX_EVENTS];
    let mut cost = [0usize; MAX_EVENTS];
    for (index, event) in events[..considered].iter().enumerate() {
        let mut counter = Counter::default();
        write_event(&mut counter, event).expect("counting cannot fail");
        cost[index] = counter.0 + 1;
        total += cost[index];
    }
    let samples_first = [true, false];
    for samples_only in samples_first {
        for index in (0..considered).rev() {
            if total <= MAX_BODY_BYTES {
                break;
            }
            let sample = matches!(events[index].kind, EventKind::ProcessSample(_));
            if keep[index] && (sample || !samples_only) {
                keep[index] = false;
                total -= cost[index];
                omitted += 1;
            }
        }
    }
    let sent = keep[..considered].iter().filter(|kept| **kept).count();
    if sent == 0 {
        return Err(EncodeError::Empty);
    }
    let dropped = window_dropped.saturating_add(u32::try_from(omitted).unwrap_or(u32::MAX));
    let capacity = body.0.capacity();
    body.0.clear();
    let out = &mut body.0;
    write_head(out, envelope, dropped).expect("writing to memory cannot fail");
    let mut first = true;
    for (event, _) in events[..considered]
        .iter()
        .zip(&keep)
        .filter(|(_, kept)| **kept)
    {
        if !first {
            out.push(b',');
        }
        first = false;
        write_event(out, event).expect("writing to memory cannot fail");
    }
    out.extend_from_slice(TAIL);
    debug_assert!(out.len() <= MAX_BODY_BYTES);
    debug_assert_eq!(out.capacity(), capacity, "the body never grows");
    Ok(Encoded {
        sent,
        omitted,
        dropped,
    })
}

const TAIL: &[u8] = b"]}";

fn write_head(w: &mut impl Write, envelope: &Envelope<'_>, dropped: u32) -> io::Result<()> {
    write!(
        w,
        "{{\"schema\":{SCHEMA},\"disclosure_version\":{},\"release\":\"{}\",\"installation_id\":\"",
        super::DISCLOSURE_VERSION,
        envelope.release.as_str()
    )?;
    w.write_all(&envelope.installation_id.to_wire())?;
    w.write_all(b"\",\"session_id\":\"")?;
    w.write_all(&envelope.session_id.to_wire())?;
    w.write_all(b"\",\"session_start\":\"")?;
    w.write_all(&envelope.session_start.to_wire())?;
    write!(
        w,
        "\",\"batch_seq\":{},\"dropped\":{dropped},\"device\":",
        envelope.batch_seq
    )?;
    envelope.device.write_json(w)?;
    w.write_all(b",\"events\":[")
}

fn write_image(w: &mut impl Write, image: &ImageId) -> io::Result<()> {
    w.write_all(b",\"image\":\"")?;
    w.write_all(&image.to_hex())?;
    w.write_all(b"\"")
}

fn write_event(w: &mut impl Write, event: &Event) -> io::Result<()> {
    write!(
        w,
        "{{\"seq\":{},\"t\":{},\"name\":\"{}\"",
        event.seq,
        event.t,
        event.kind.name().as_str()
    )?;
    match &event.kind {
        EventKind::SessionStart { trigger } => write!(w, ",\"trigger\":\"{}\"", trigger.as_str())?,
        EventKind::AppPresented { state } => write!(w, ",\"state\":\"{}\"", state.as_str())?,
        EventKind::AppIdle { state } => write!(w, ",\"state\":\"{}\"", state.as_str())?,
        EventKind::WorkspaceEnter { workspace } => {
            write!(w, ",\"workspace\":\"{}\"", workspace.as_str())?
        }
        EventKind::ImageOpen { image } => write_image(w, image)?,
        EventKind::Interaction { feature, image } => {
            write!(w, ",\"feature\":\"{}\"", feature.as_str())?;
            if let Some(image) = image {
                write_image(w, image)?;
            }
        }
        EventKind::ToolCommit { tool, image } => {
            write!(w, ",\"tool\":\"{}\"", tool.as_str())?;
            write_image(w, image)?;
        }
        EventKind::ImagesAdded { count } => write!(w, ",\"count\":{}", count.get())?,
        EventKind::OperationEnd(end) => {
            write!(
                w,
                ",\"operation\":\"{}\",\"outcome\":\"{}\",\"duration_ms\":{}",
                end.operation().as_str(),
                end.outcome().as_str(),
                end.duration().get()
            )?;
            if let Some(count) = end.count() {
                write!(w, ",\"count\":{count}")?;
            }
            if let Some(image) = end.image() {
                write_image(w, &image)?;
            }
        }
        EventKind::PreviewSettled {
            image,
            renderer,
            duration,
        } => {
            write_image(w, image)?;
            write!(
                w,
                ",\"renderer\":\"{}\",\"duration_ms\":{}",
                renderer.as_str(),
                duration.get()
            )?;
        }
        EventKind::ProcessSample(sample) => {
            write!(
                w,
                ",\"cpu_percent\":{},\"memory_mib\":{},\"memory_kind\":\"{}\"",
                sample.cpu.get(),
                sample.memory.get(),
                sample.memory_kind.as_str()
            )?;
            if let Some(gpu) = sample.gpu {
                write!(w, ",\"gpu_percent\":{}", gpu.get())?;
            }
            if let Some(memory) = sample.gpu_memory {
                write!(w, ",\"gpu_memory_mib\":{}", memory.get())?;
            }
        }
    }
    w.write_all(b"}")
}

// --- The strict decoder ---------------------------------------------------------------------

/// Why a body is not a valid batch. The reason names keys and rules, never a received value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaError(String);

impl SchemaError {
    pub(super) fn new(reason: impl Into<String>) -> Self {
        Self(reason.into())
    }

    pub fn reason(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SchemaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SchemaError {}

/// A parsed JSON value that kept only what the format can hold: non-negative integers, and
/// objects as ordered key lists so a duplicate key is refused rather than silently overwritten.
#[derive(Debug)]
pub(super) enum Json {
    Null,
    Bool(bool),
    Int(u64),
    /// A negative or fractional number, which no field allows.
    OtherNumber,
    Str(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl<'de> Deserialize<'de> for Json {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct JsonVisitor;
        impl<'de> Visitor<'de> for JsonVisitor {
            type Value = Json;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("JSON")
            }
            fn visit_unit<E>(self) -> Result<Json, E> {
                Ok(Json::Null)
            }
            fn visit_bool<E>(self, value: bool) -> Result<Json, E> {
                Ok(Json::Bool(value))
            }
            fn visit_u64<E>(self, value: u64) -> Result<Json, E> {
                Ok(Json::Int(value))
            }
            fn visit_i64<E>(self, value: i64) -> Result<Json, E> {
                Ok(u64::try_from(value).map_or(Json::OtherNumber, Json::Int))
            }
            fn visit_f64<E>(self, _: f64) -> Result<Json, E> {
                Ok(Json::OtherNumber)
            }
            fn visit_str<E>(self, value: &str) -> Result<Json, E> {
                Ok(Json::Str(value.to_owned()))
            }
            fn visit_string<E>(self, value: String) -> Result<Json, E> {
                Ok(Json::Str(value))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Json, A::Error> {
                let mut items = Vec::new();
                while let Some(item) = seq.next_element()? {
                    items.push(item);
                }
                Ok(Json::Array(items))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Json, A::Error> {
                let mut entries: Vec<(String, Json)> = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    if entries.iter().any(|(held, _)| *held == key) {
                        return Err(de::Error::custom("a key repeats"));
                    }
                    let value = map.next_value()?;
                    entries.push((key, value));
                }
                Ok(Json::Object(entries))
            }
        }
        deserializer.deserialize_any(JsonVisitor)
    }
}

/// An object's fields after its keys were checked against a closed set.
pub(super) struct Fields<'a>(&'a [(String, Json)]);

impl<'a> Fields<'a> {
    pub(super) fn get(&self, key: &str) -> Option<&'a Json> {
        self.0
            .iter()
            .find(|(held, _)| held == key)
            .map(|(_, value)| value)
    }

    pub(super) fn req(&self, key: &str) -> Result<&'a Json, SchemaError> {
        self.get(key)
            .ok_or_else(|| SchemaError::new(format!("{key} is required")))
    }
}

/// `json` as an object holding every `required` key, any `optional` ones and nothing else.
pub(super) fn closed<'a>(
    json: &'a Json,
    required: &[&str],
    optional: &[&str],
    what: &str,
) -> Result<Fields<'a>, SchemaError> {
    let Json::Object(entries) = json else {
        return Err(SchemaError::new(format!("{what} is an object")));
    };
    for (key, _) in entries {
        if !required.contains(&key.as_str()) && !optional.contains(&key.as_str()) {
            return Err(SchemaError::new(format!("{what} has an unknown key")));
        }
    }
    for key in required {
        if !entries.iter().any(|(held, _)| held == key) {
            return Err(SchemaError::new(format!("{what}.{key} is required")));
        }
    }
    Ok(Fields(entries))
}

/// `json` as an integer in `0..=max`.
pub(super) fn int(json: &Json, max: u64, what: &str) -> Result<u64, SchemaError> {
    match json {
        Json::Int(value) if *value <= max => Ok(*value),
        Json::Int(_) => Err(SchemaError::new(format!("{what} is out of range"))),
        _ => Err(SchemaError::new(format!(
            "{what} is a non-negative integer"
        ))),
    }
}

fn string<'a>(json: &'a Json, what: &str) -> Result<&'a str, SchemaError> {
    match json {
        Json::Str(text) => Ok(text),
        _ => Err(SchemaError::new(format!("{what} is a string"))),
    }
}

/// The depth of nested containers, the envelope included: envelope → device → gpus → gpu object →
/// `driver_version` is 5, the most the format holds.
fn depth(json: &Json) -> usize {
    match json {
        Json::Object(entries) => 1 + entries.iter().map(|(_, v)| depth(v)).max().unwrap_or(0),
        Json::Array(items) => 1 + items.iter().map(depth).max().unwrap_or(0),
        _ => 0,
    }
}

/// A batch the decoder accepted, as typed values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Batch {
    pub release: Release,
    pub installation_id: ReportId,
    pub session_id: ReportId,
    pub session_start: SessionStart,
    pub batch_seq: u32,
    pub dropped: u32,
    pub device: DeviceProfile,
    pub events: Vec<Event>,
}

/// Validate `body` as exactly one batch of the current schema: the body and event limits, closed
/// objects with no repeated key, exact enums and ranges, canonical UUIDv4s, a canonical session
/// start, the device cap, each event's allowed fields, strictly increasing `seq` and non-decreasing
/// `t`. Any violation refuses the whole batch.
pub fn decode_batch(body: &[u8]) -> Result<Batch, SchemaError> {
    if body.len() > MAX_BODY_BYTES {
        return Err(SchemaError::new("the body exceeds 65536 bytes"));
    }
    let json: Json = serde_json::from_slice(body)
        .map_err(|_| SchemaError::new("the body is not one JSON document"))?;
    if depth(&json) > 5 {
        return Err(SchemaError::new(
            "the body nests deeper than 4 below the envelope",
        ));
    }
    let envelope = closed(
        &json,
        &[
            "schema",
            "disclosure_version",
            "release",
            "installation_id",
            "session_id",
            "session_start",
            "batch_seq",
            "dropped",
            "device",
            "events",
        ],
        &[],
        "envelope",
    )?;
    if int(envelope.req("schema")?, u64::MAX, "schema")? != u64::from(SCHEMA) {
        return Err(SchemaError::new("schema is not 1"));
    }
    if int(
        envelope.req("disclosure_version")?,
        u64::MAX,
        "disclosure_version",
    )? != u64::from(super::DISCLOSURE_VERSION)
    {
        return Err(SchemaError::new("disclosure_version is not 1"));
    }
    let release = Release::parse(string(envelope.req("release")?, "release")?)
        .ok_or_else(|| SchemaError::new("release has the version+revision shape"))?;
    let id = |key: &str| -> Result<ReportId, SchemaError> {
        ReportId::parse(string(envelope.req(key)?, key)?)
            .ok_or_else(|| SchemaError::new(format!("{key} is a canonical UUIDv4")))
    };
    let installation_id = id("installation_id")?;
    let session_id = id("session_id")?;
    let session_start =
        SessionStart::parse(string(envelope.req("session_start")?, "session_start")?).ok_or_else(
            || SchemaError::new("session_start is a canonical UTC second, 2020 to 2200"),
        )?;
    let batch_seq = int(envelope.req("batch_seq")?, u64::from(u32::MAX), "batch_seq")? as u32;
    let dropped = int(envelope.req("dropped")?, u64::from(u32::MAX), "dropped")? as u32;
    let device = DeviceProfile::decode(envelope.req("device")?)?;
    if device.encoded_len() > MAX_DEVICE_BYTES {
        return Err(SchemaError::new("device exceeds 2048 bytes"));
    }
    let Json::Array(items) = envelope.req("events")? else {
        return Err(SchemaError::new("events is an array"));
    };
    if items.is_empty() || items.len() > MAX_EVENTS {
        return Err(SchemaError::new("events holds 1 to 512 events"));
    }
    let mut events = Vec::with_capacity(items.len());
    for item in items {
        let event = decode_event(item)?;
        if let Some(previous) = events.last().map(|e: &Event| (e.seq, e.t)) {
            if event.seq <= previous.0 {
                return Err(SchemaError::new("seq does not strictly increase"));
            }
            if event.t < previous.1 {
                return Err(SchemaError::new("t decreases"));
            }
        }
        events.push(event);
    }
    Ok(Batch {
        release,
        installation_id,
        session_id,
        session_start,
        batch_seq,
        dropped,
        device,
        events,
    })
}

fn decode_event(json: &Json) -> Result<Event, SchemaError> {
    let Json::Object(entries) = json else {
        return Err(SchemaError::new("an event is an object"));
    };
    let name = entries
        .iter()
        .find(|(key, _)| key == "name")
        .ok_or_else(|| SchemaError::new("an event has a name"))
        .and_then(|(_, name)| enum_of(name, EventName::parse, "event name"))?;
    let (required, optional): (&[&str], &[&str]) = match name {
        EventName::SessionStart => (&["trigger"], &[]),
        EventName::AppPresented | EventName::AppIdle => (&["state"], &[]),
        EventName::WorkspaceEnter => (&["workspace"], &[]),
        EventName::ImageOpen => (&["image"], &[]),
        EventName::Interaction => (&["feature"], &["image"]),
        EventName::ToolCommit => (&["tool", "image"], &[]),
        EventName::ImagesAdded => (&["count"], &[]),
        EventName::OperationEnd => (
            &["operation", "outcome", "duration_ms"],
            &["count", "image"],
        ),
        EventName::PreviewSettled => (&["image", "renderer", "duration_ms"], &[]),
        EventName::ProcessSample => (
            &["cpu_percent", "memory_mib", "memory_kind"],
            &["gpu_percent", "gpu_memory_mib"],
        ),
    };
    let mut all_required = vec!["seq", "t", "name"];
    all_required.extend_from_slice(required);
    let fields = closed(json, &all_required, optional, name.as_str())?;
    let seq = int(fields.req("seq")?, u64::from(u32::MAX), "seq")? as u32;
    let t = int(fields.req("t")?, u64::from(u32::MAX), "t")? as u32;
    let image = |key: &str| -> Result<Option<ImageId>, SchemaError> {
        fields
            .get(key)
            .map(|value| {
                ImageId::parse(string(value, "image")?)
                    .ok_or_else(|| SchemaError::new("image is 32 lowercase hex characters"))
            })
            .transpose()
    };
    let required_image = || -> Result<ImageId, SchemaError> {
        image("image")?.ok_or_else(|| SchemaError::new("image is required"))
    };
    let duration = || -> Result<DurationMs, SchemaError> {
        let millis = int(fields.req("duration_ms")?, u64::MAX, "duration_ms")?;
        DurationMs::exact(millis).ok_or_else(|| SchemaError::new("duration_ms is out of range"))
    };
    let kind = match name {
        EventName::SessionStart => EventKind::SessionStart {
            trigger: enum_of(fields.req("trigger")?, Trigger::parse, "trigger")?,
        },
        EventName::AppPresented => EventKind::AppPresented {
            state: enum_of(fields.req("state")?, Presented::parse, "state")?,
        },
        EventName::AppIdle => EventKind::AppIdle {
            state: enum_of(fields.req("state")?, Idle::parse, "state")?,
        },
        EventName::WorkspaceEnter => EventKind::WorkspaceEnter {
            workspace: enum_of(fields.req("workspace")?, Workspace::parse, "workspace")?,
        },
        EventName::ImageOpen => EventKind::ImageOpen {
            image: required_image()?,
        },
        EventName::Interaction => EventKind::Interaction {
            feature: enum_of(fields.req("feature")?, Feature::parse, "feature")?,
            image: image("image")?,
        },
        EventName::ToolCommit => EventKind::ToolCommit {
            tool: enum_of(fields.req("tool")?, Tool::parse, "tool")?,
            image: required_image()?,
        },
        EventName::ImagesAdded => EventKind::ImagesAdded {
            count: AddedCount::exact(int(fields.req("count")?, u64::MAX, "count")?)
                .ok_or_else(|| SchemaError::new("count is 1 to 1000000"))?,
        },
        EventName::OperationEnd => {
            let operation = enum_of(fields.req("operation")?, Operation::parse, "operation")?;
            let count = fields
                .get("count")
                .map(|value| int(value, u64::MAX, "count"))
                .transpose()?;
            EventKind::OperationEnd(
                OperationEnd::checked(
                    operation,
                    enum_of(fields.req("outcome")?, Outcome::parse, "outcome")?,
                    duration()?,
                    count,
                    image("image")?,
                )
                .ok_or_else(|| {
                    SchemaError::new("operation.end carries a field its operation may not")
                })?,
            )
        }
        EventName::PreviewSettled => EventKind::PreviewSettled {
            image: required_image()?,
            renderer: enum_of(fields.req("renderer")?, Renderer::parse, "renderer")?,
            duration: duration()?,
        },
        EventName::ProcessSample => {
            let percent = |key: &str| -> Result<Option<Percent>, SchemaError> {
                fields
                    .get(key)
                    .map(|value| {
                        Percent::exact(int(value, u64::MAX, key)?)
                            .ok_or_else(|| SchemaError::new(format!("{key} is out of range")))
                    })
                    .transpose()
            };
            let mib = |key: &str| -> Result<Option<MemoryMib>, SchemaError> {
                fields
                    .get(key)
                    .map(|value| {
                        MemoryMib::exact(int(value, u64::MAX, key)?)
                            .ok_or_else(|| SchemaError::new(format!("{key} is out of range")))
                    })
                    .transpose()
            };
            EventKind::ProcessSample(ProcessSample {
                cpu: percent("cpu_percent")?.expect("required"),
                memory: mib("memory_mib")?.expect("required"),
                memory_kind: enum_of(fields.req("memory_kind")?, MemoryKind::parse, "memory_kind")?,
                gpu: percent("gpu_percent")?,
                gpu_memory: mib("gpu_memory_mib")?,
            })
        }
    };
    Ok(Event { seq, t, kind })
}

#[cfg(test)]
#[path = "encode_tests.rs"]
mod tests;
