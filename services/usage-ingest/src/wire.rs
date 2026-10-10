//! Strict decoding of one usage batch, exactly as `docs/design/usage-statistics-wire.md` fixes it.
//!
//! Every object is closed: an unknown key, a duplicate key, a wrong type, an out-of-range value or
//! a mis-ordered sequence rejects the whole batch. Nothing here reports *why* a batch was refused
//! beyond [`Invalid`], so no input can reach a log through an error message. Every string that
//! leaves this module is either one of this module's own `'static` enum words, or a value that
//! passed a fixed format check (identifiers, release, image, session start).

use crate::registry::Registry;
use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::{Serialize, Serializer};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// The body limit, in bytes.
pub const MAX_BODY: usize = 65_536;
/// The most nested containers a body may have, counting the envelope as one: envelope, device,
/// gpus array, gpu object, and a gpu's `driver_version` array. (The wire format's "nesting depth at
/// most 4" counts levels below the envelope; the schema's deepest value is the 5th container.)
pub const MAX_DEPTH: usize = 5;
/// The most events in one batch.
pub const MAX_EVENTS: usize = 512;
/// The device object's serialized size limit, in bytes.
pub const MAX_DEVICE: usize = 2048;

const U32_MAX: u64 = u32::MAX as u64;

/// A batch is refused; the reason is deliberately not recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Invalid;

type R<T> = Result<T, Invalid>;

/// A parsed JSON value that keeps only what validation needs. Duplicate keys, which `serde_json`
/// would resolve silently, are an error here.
#[derive(Debug)]
enum Val {
    Null,
    Bool(bool),
    Uint(u64),
    /// A negative or fractional number: never valid anywhere in the format.
    Number,
    Str(String),
    Arr(Vec<Val>),
    Obj(BTreeMap<String, Val>),
}

impl<'de> Deserialize<'de> for Val {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Val, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Val;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a JSON value")
            }
            fn visit_unit<E>(self) -> Result<Val, E> {
                Ok(Val::Null)
            }
            fn visit_bool<E>(self, v: bool) -> Result<Val, E> {
                Ok(Val::Bool(v))
            }
            fn visit_u64<E>(self, v: u64) -> Result<Val, E> {
                Ok(Val::Uint(v))
            }
            fn visit_i64<E>(self, v: i64) -> Result<Val, E> {
                Ok(u64::try_from(v).map_or(Val::Number, Val::Uint))
            }
            fn visit_f64<E>(self, _: f64) -> Result<Val, E> {
                Ok(Val::Number)
            }
            fn visit_str<E>(self, v: &str) -> Result<Val, E> {
                Ok(Val::Str(v.to_owned()))
            }
            fn visit_string<E>(self, v: String) -> Result<Val, E> {
                Ok(Val::Str(v))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Val, A::Error> {
                let mut items = Vec::new();
                while let Some(item) = seq.next_element()? {
                    items.push(item);
                }
                Ok(Val::Arr(items))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Val, A::Error> {
                let mut entries = BTreeMap::new();
                while let Some((key, value)) = map.next_entry::<String, Val>()? {
                    if entries.insert(key, value).is_some() {
                        return Err(serde::de::Error::custom("duplicate key"));
                    }
                }
                Ok(Val::Obj(entries))
            }
        }
        deserializer.deserialize_any(V)
    }
}

impl Serialize for Val {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Val::Null | Val::Number => serializer.serialize_unit(),
            Val::Bool(b) => serializer.serialize_bool(*b),
            Val::Uint(n) => serializer.serialize_u64(*n),
            Val::Str(s) => serializer.serialize_str(s),
            Val::Arr(items) => serializer.collect_seq(items),
            Val::Obj(map) => serializer.collect_map(map),
        }
    }
}

/// Whether `body` nests containers no deeper than `limit`, reading it as JSON text (strings and
/// their escapes included). Run before parsing, so a deep body never reaches the parser.
fn shallow(body: &[u8], limit: usize) -> bool {
    let (mut depth, mut in_string, mut escaped) = (0usize, false, false);
    for &byte in body {
        if in_string {
            match (escaped, byte) {
                (true, _) => escaped = false,
                (false, b'\\') => escaped = true,
                (false, b'"') => in_string = false,
                _ => {}
            }
        } else {
            match byte {
                b'"' => in_string = true,
                b'{' | b'[' => {
                    depth += 1;
                    if depth > limit {
                        return false;
                    }
                }
                b'}' | b']' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
    }
    true
}

/// A closed object being read: every key must be taken exactly once before [`Obj::finish`].
struct Obj<'a> {
    map: &'a BTreeMap<String, Val>,
    taken: BTreeSet<&'a str>,
}

impl<'a> Obj<'a> {
    fn new(value: &'a Val) -> R<Obj<'a>> {
        match value {
            Val::Obj(map) => Ok(Obj {
                map,
                taken: BTreeSet::new(),
            }),
            _ => Err(Invalid),
        }
    }

    fn take(&mut self, key: &'static str) -> Option<&'a Val> {
        let (name, value) = self.map.get_key_value(key)?;
        self.taken.insert(name.as_str());
        Some(value)
    }

    fn required(&mut self, key: &'static str) -> R<&'a Val> {
        self.take(key).ok_or(Invalid)
    }

    fn finish(self) -> R<()> {
        if self.map.keys().all(|key| self.taken.contains(key.as_str())) {
            Ok(())
        } else {
            Err(Invalid)
        }
    }

    fn uint(&mut self, key: &'static str, low: u64, high: u64) -> R<u32> {
        uint(self.required(key)?, low, high)
    }

    fn opt_uint(&mut self, key: &'static str, low: u64, high: u64) -> R<Option<u32>> {
        self.take(key).map(|v| uint(v, low, high)).transpose()
    }

    fn choice(&mut self, key: &'static str, words: &[&'static str]) -> R<&'static str> {
        choice(self.required(key)?, words)
    }

    fn opt_choice(&mut self, key: &'static str, words: &[&'static str]) -> R<Option<&'static str>> {
        self.take(key).map(|v| choice(v, words)).transpose()
    }

    fn text(&mut self, key: &'static str) -> R<&'a str> {
        text(self.required(key)?)
    }

    fn image(&mut self, key: &'static str) -> R<String> {
        let image = self.text(key)?;
        lower_hex(image, 32)
            .then(|| image.to_owned())
            .ok_or(Invalid)
    }

    fn opt_image(&mut self, key: &'static str) -> R<Option<String>> {
        if self.map.contains_key(key) {
            self.image(key).map(Some)
        } else {
            Ok(None)
        }
    }
}

fn uint(value: &Val, low: u64, high: u64) -> R<u32> {
    match value {
        Val::Uint(n) if (low..=high).contains(n) => u32::try_from(*n).map_err(|_| Invalid),
        _ => Err(Invalid),
    }
}

fn text(value: &Val) -> R<&str> {
    match value {
        Val::Str(s) => Ok(s),
        _ => Err(Invalid),
    }
}

fn choice(value: &Val, words: &[&'static str]) -> R<&'static str> {
    let word = text(value)?;
    words.iter().copied().find(|w| *w == word).ok_or(Invalid)
}

fn flag(value: &Val) -> R<bool> {
    match value {
        Val::Bool(b) => Ok(*b),
        _ => Err(Invalid),
    }
}

fn lower_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A canonical lowercase hyphenated UUID, version 4, RFC 4122 variant.
fn uuid_v4(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(i, &b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            14 => b == b'4',
            19 => matches!(b, b'8' | b'9' | b'a' | b'b'),
            _ => matches!(b, b'0'..=b'9' | b'a'..=b'f'),
        })
}

/// `^(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})\+[0-9a-f]{12}$`
fn release(value: &str) -> bool {
    let Some((version, revision)) = value.split_once('+') else {
        return false;
    };
    let parts: Vec<&str> = version.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.len() <= 9
                && p.bytes().all(|b| b.is_ascii_digit())
                && (*p == "0" || !p.starts_with('0'))
        })
        && lower_hex(revision, 12)
}

fn leap(year: u32) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

/// `YYYY-MM-DDTHH:MM:SSZ`, a valid UTC calendar time in 2020..=2200 (no leap second).
fn session_start(value: &str) -> bool {
    let b = value.as_bytes();
    if b.len() != 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'Z'
    {
        return false;
    }
    let number = |from: usize, to: usize| -> Option<u32> {
        let digits = &b[from..to];
        digits
            .iter()
            .all(u8::is_ascii_digit)
            .then(|| digits.iter().fold(0, |n, d| n * 10 + u32::from(d - b'0')))
    };
    let (Some(year), Some(month), Some(day), Some(hour), Some(minute), Some(second)) = (
        number(0, 4),
        number(5, 7),
        number(8, 10),
        number(11, 13),
        number(14, 16),
        number(17, 19),
    ) else {
        return false;
    };
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap(year) => 29,
        2 => 28,
        _ => return false,
    };
    (2020..=2200).contains(&year)
        && (1..=days).contains(&day)
        && hour < 24
        && minute < 60
        && second < 60
}

fn macos_build(value: &str) -> bool {
    // ^[0-9]{1,3}[A-Z][0-9]{1,5}[a-z]?$
    let b = value.as_bytes();
    let digits = |from: usize| b[from..].iter().take_while(|c| c.is_ascii_digit()).count();
    let lead = digits(0);
    if !(1..=3).contains(&lead) || b.get(lead).is_none_or(|c| !c.is_ascii_uppercase()) {
        return false;
    }
    let tail = digits(lead + 1);
    if !(1..=5).contains(&tail) {
        return false;
    }
    match &b[lead + 1 + tail..] {
        [] => true,
        [c] => c.is_ascii_lowercase(),
        _ => false,
    }
}

/// One to four integers `0..=4294967295`.
fn version(value: &Val) -> R<Vec<u32>> {
    match value {
        Val::Arr(items) if (1..=4).contains(&items.len()) => {
            items.iter().map(|v| uint(v, 0, U32_MAX)).collect()
        }
        _ => Err(Invalid),
    }
}

fn version_value(numbers: &[u32]) -> Value {
    Value::from(numbers.to_vec())
}

const OS_FAMILIES: &[&str] = &["macos", "windows", "linux", "other"];
const DISTROS: &[&str] = &[
    "arch", "debian", "ubuntu", "fedora", "rhel", "opensuse", "nixos", "alpine", "other",
];
const ARCHES: &[&str] = &["arm64", "x86_64", "other"];
const CPU_VENDORS: &[&str] = &["apple", "intel", "amd", "qualcomm", "other"];
const TIERS: &[&str] = &["base", "pro", "max", "ultra"];
const GPU_VENDORS: &[&str] = &[
    "apple",
    "nvidia",
    "amd",
    "intel",
    "qualcomm",
    "arm",
    "imagination",
    "microsoft",
    "mesa",
    "other",
];
const GPU_BACKENDS: &[&str] = &["metal", "vulkan", "dx12", "gl", "other"];
const GPU_TYPES: &[&str] = &["integrated", "discrete", "virtual", "cpu", "other"];
const RENDERERS: &[&str] = &["gpu", "reference"];
const FALLBACKS: &[&str] = &[
    "no-adapter",
    "adapter-lost",
    "unsupported",
    "disabled",
    "other",
];
const MEMORY_BANDS: &[u64] = &[8, 16, 32, 64, 128, 256, 512, 1024];

/// The device profile's per-GPU fields, in column order. A GPU's column is `gpu{0,1}_{field}`.
pub const GPU_FIELDS: &[&str] = &[
    "vendor",
    "backend",
    "type",
    "unified_memory",
    "vendor_id",
    "device_id",
    "generation",
    "tier",
    "memory_gib_band",
    "driver_version",
];

/// The device profile's columns after the four identifying ones, in order.
pub fn profile_columns() -> Vec<String> {
    let mut columns: Vec<String> = [
        "os_family",
        "os_version",
        "os_build",
        "os_distro",
        "os_distro_version",
        "os_kernel",
        "arch",
        "cpu_vendor",
        "cpu_physical_cores",
        "cpu_logical_cores",
        "cpu_generation",
        "cpu_tier",
        "cpu_family",
        "cpu_model",
        "cpu_stepping",
        "memory_gib_band",
        "gpu_count",
    ]
    .map(String::from)
    .to_vec();
    for gpu in 0..2 {
        columns.extend(GPU_FIELDS.iter().map(|field| format!("gpu{gpu}_{field}")));
    }
    columns.extend(["renderer", "renderer_fallback"].map(String::from));
    columns
}

/// A validated device profile, as its flattened columns. Absent fields are `null`.
#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    pub columns: BTreeMap<String, Value>,
}

fn memory_band(value: &Val) -> R<Value> {
    match value {
        Val::Uint(n) if MEMORY_BANDS.contains(n) => Ok(Value::from(n.to_string())),
        Val::Str(s) if s == "over1024" => Ok(Value::from("over1024")),
        _ => Err(Invalid),
    }
}

fn device(value: &Val) -> R<Device> {
    // The serialized size is the compact form, so whitespace in the body cannot change the verdict.
    let size = serde_json::to_vec(value).map_err(|_| Invalid)?.len();
    if size > MAX_DEVICE {
        return Err(Invalid);
    }
    let mut columns: BTreeMap<String, Value> = profile_columns()
        .into_iter()
        .map(|name| (name, Value::Null))
        .collect();
    let mut put = |name: &str, value: Value| {
        columns.insert(name.to_owned(), value);
    };
    let mut d = Obj::new(value)?;

    let mut os = Obj::new(d.required("os")?)?;
    let family = os.choice("family", OS_FAMILIES)?;
    put("os_family", family.into());
    put(
        "os_version",
        version_value(&version(os.required("version")?)?),
    );
    if let Some(build) = os.take("build") {
        if family != "macos" || !macos_build(text(build)?) {
            return Err(Invalid);
        }
        put("os_build", text(build)?.into());
    }
    let linux_only = (os.map.contains_key("distro"))
        || os.map.contains_key("distro_version")
        || os.map.contains_key("kernel");
    if linux_only && family != "linux" {
        return Err(Invalid);
    }
    if let Some(distro) = os.opt_choice("distro", DISTROS)? {
        put("os_distro", distro.into());
    }
    if let Some(v) = os.take("distro_version") {
        put("os_distro_version", version_value(&version(v)?));
    }
    if let Some(v) = os.take("kernel") {
        put("os_kernel", version_value(&version(v)?));
    }
    os.finish()?;

    put("arch", d.choice("arch", ARCHES)?.into());

    let mut cpu = Obj::new(d.required("cpu")?)?;
    let vendor = cpu.choice("vendor", CPU_VENDORS)?;
    put("cpu_vendor", vendor.into());
    put(
        "cpu_physical_cores",
        cpu.uint("physical_cores", 1, 1024)?.into(),
    );
    put(
        "cpu_logical_cores",
        cpu.uint("logical_cores", 1, 1024)?.into(),
    );
    let apple_only = cpu.map.contains_key("generation") || cpu.map.contains_key("tier");
    if apple_only && vendor != "apple" {
        return Err(Invalid);
    }
    if let Some(generation) = cpu.opt_uint("generation", 1, 64)? {
        put("cpu_generation", generation.into());
    }
    if let Some(tier) = cpu.opt_choice("tier", TIERS)? {
        put("cpu_tier", tier.into());
    }
    let x86_only = ["family", "model", "stepping"]
        .iter()
        .any(|key| cpu.map.contains_key(*key));
    if x86_only && !matches!(vendor, "intel" | "amd") {
        return Err(Invalid);
    }
    for (key, column) in [
        ("family", "cpu_family"),
        ("model", "cpu_model"),
        ("stepping", "cpu_stepping"),
    ] {
        if let Some(n) = cpu.opt_uint(key, 0, 4095)? {
            put(column, n.into());
        }
    }
    cpu.finish()?;

    put(
        "memory_gib_band",
        memory_band(d.required("memory_gib_band")?)?,
    );

    let Val::Arr(gpus) = d.required("gpus")? else {
        return Err(Invalid);
    };
    if gpus.len() > 2 {
        return Err(Invalid);
    }
    put("gpu_count", Value::from(gpus.len()));
    let mut identities = Vec::new();
    for (index, gpu) in gpus.iter().enumerate() {
        let mut g = Obj::new(gpu)?;
        let name = |field: &str| format!("gpu{index}_{field}");
        let vendor = g.choice("vendor", GPU_VENDORS)?;
        put(&name("vendor"), vendor.into());
        put(&name("backend"), g.choice("backend", GPU_BACKENDS)?.into());
        put(&name("type"), g.choice("type", GPU_TYPES)?.into());
        put(
            &name("unified_memory"),
            flag(g.required("unified_memory")?)?.into(),
        );
        let vendor_id = g.opt_uint("vendor_id", 0, 65_535)?;
        let device_id = g.opt_uint("device_id", 0, 65_535)?;
        let apple_only = g.map.contains_key("generation") || g.map.contains_key("tier");
        if apple_only && vendor != "apple" {
            return Err(Invalid);
        }
        let generation = g.opt_uint("generation", 1, 64)?;
        let tier = g.opt_choice("tier", TIERS)?;
        for (field, value) in [
            ("vendor_id", vendor_id),
            ("device_id", device_id),
            ("generation", generation),
        ] {
            if let Some(n) = value {
                put(&name(field), n.into());
            }
        }
        if let Some(tier) = tier {
            put(&name("tier"), tier.into());
        }
        if let Some(band) = g.take("memory_gib_band") {
            put(&name("memory_gib_band"), memory_band(band)?);
        }
        if let Some(v) = g.take("driver_version") {
            put(&name("driver_version"), version_value(&version(v)?));
        }
        g.finish()?;
        let identity = (vendor, vendor_id, device_id, generation, tier);
        if identities.contains(&identity) {
            return Err(Invalid);
        }
        identities.push(identity);
    }

    let renderer = d.choice("renderer", RENDERERS)?;
    put("renderer", renderer.into());
    if let Some(fallback) = d.opt_choice("renderer_fallback", FALLBACKS)? {
        if renderer != "reference" {
            return Err(Invalid);
        }
        put("renderer_fallback", fallback.into());
    }
    d.finish()?;
    Ok(Device { columns })
}

/// An event's fields: one column each, `None` where the event does not carry it.
#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct EventFields {
    pub trigger: Option<&'static str>,
    pub state: Option<&'static str>,
    pub workspace: Option<&'static str>,
    pub image: Option<String>,
    pub feature: Option<&'static str>,
    pub tool: Option<&'static str>,
    pub count: Option<u32>,
    pub operation: Option<&'static str>,
    pub outcome: Option<&'static str>,
    pub duration_ms: Option<u32>,
    pub renderer: Option<&'static str>,
    pub cpu_percent: Option<u32>,
    pub memory_mib: Option<u32>,
    pub memory_kind: Option<&'static str>,
    pub gpu_percent: Option<u32>,
    pub gpu_memory_mib: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub seq: u32,
    pub t: u32,
    pub name: &'static str,
    pub fields: EventFields,
}

const MAX_DURATION_MS: u64 = 86_400_000;
const MAX_COUNT: u64 = 1_000_000;
const MAX_PERCENT: u64 = 102_400;
const MAX_MIB: u64 = 16_777_216;

fn event(value: &Val, registry: &'static Registry) -> R<Event> {
    let mut o = Obj::new(value)?;
    let seq = o.uint("seq", 0, U32_MAX)?;
    let t = o.uint("t", 0, U32_MAX)?;
    let name = o.text("name")?;
    let mut f = EventFields::default();
    let name = match name {
        "session.start" => {
            f.trigger = Some(o.choice("trigger", &["launch", "opt-in"])?);
            "session.start"
        }
        "app.presented" => {
            f.state = Some(o.choice("state", &["visible", "hidden"])?);
            "app.presented"
        }
        "app.idle" => {
            f.state = Some(o.choice("state", &["idle", "active"])?);
            "app.idle"
        }
        "workspace.enter" => {
            f.workspace = Some(o.choice("workspace", &["select", "develop"])?);
            "workspace.enter"
        }
        "image.open" => {
            f.image = Some(o.image("image")?);
            "image.open"
        }
        "interaction" => {
            let feature = o.text("feature")?;
            f.feature = Some(registry.feature(feature).ok_or(Invalid)?);
            f.image = o.opt_image("image")?;
            "interaction"
        }
        "tool.commit" => {
            let tool = o.text("tool")?;
            f.tool = Some(registry.tool(tool).ok_or(Invalid)?);
            f.image = Some(o.image("image")?);
            "tool.commit"
        }
        "images.added" => {
            f.count = Some(o.uint("count", 1, MAX_COUNT)?);
            "images.added"
        }
        "operation.end" => {
            let operation = o.choice(
                "operation",
                &["source-preparation", "catalog-scan", "export", "auto-tone"],
            )?;
            f.operation = Some(operation);
            f.outcome =
                Some(o.choice("outcome", &["completed", "cancelled", "refused", "failed"])?);
            f.duration_ms = Some(o.uint("duration_ms", 0, MAX_DURATION_MS)?);
            if operation == "export" {
                f.count = o.opt_uint("count", 0, MAX_COUNT)?;
            }
            if operation != "catalog-scan" {
                f.image = o.opt_image("image")?;
            }
            "operation.end"
        }
        "preview.settled" => {
            f.image = Some(o.image("image")?);
            f.renderer = Some(o.choice("renderer", RENDERERS)?);
            f.duration_ms = Some(o.uint("duration_ms", 0, MAX_DURATION_MS)?);
            "preview.settled"
        }
        "process.sample" => {
            f.cpu_percent = Some(o.uint("cpu_percent", 0, MAX_PERCENT)?);
            f.memory_mib = Some(o.uint("memory_mib", 0, MAX_MIB)?);
            f.memory_kind = Some(o.choice("memory_kind", &["footprint", "resident", "private"])?);
            f.gpu_percent = o.opt_uint("gpu_percent", 0, MAX_PERCENT)?;
            f.gpu_memory_mib = o.opt_uint("gpu_memory_mib", 0, MAX_MIB)?;
            "process.sample"
        }
        _ => return Err(Invalid),
    };
    o.finish()?;
    Ok(Event {
        seq,
        t,
        name,
        fields: f,
    })
}

/// A validated batch. Strings in it are identifiers and enum words, never free text.
#[derive(Debug, Clone, PartialEq)]
pub struct Batch {
    pub release: String,
    pub installation_id: String,
    pub session_id: String,
    pub session_start: String,
    pub batch_seq: u32,
    pub dropped: u32,
    pub device: Device,
    pub events: Vec<Event>,
}

/// Decode and validate a request body with the compiled-in registry.
pub fn parse(body: &[u8]) -> Result<Batch, Invalid> {
    parse_with(body, Registry::get())
}

pub fn parse_with(body: &[u8], registry: &'static Registry) -> Result<Batch, Invalid> {
    if body.len() > MAX_BODY || !shallow(body, MAX_DEPTH) {
        return Err(Invalid);
    }
    let value: Val = serde_json::from_slice(body).map_err(|_| Invalid)?;
    let mut o = Obj::new(&value)?;
    o.uint("schema", 1, 1)?;
    o.uint("disclosure_version", 1, 1)?;
    let release_text = o.text("release")?;
    if !release(release_text) {
        return Err(Invalid);
    }
    let id = |o: &mut Obj, key: &'static str| -> R<String> {
        let id = o.text(key)?;
        uuid_v4(id).then(|| id.to_owned()).ok_or(Invalid)
    };
    let installation_id = id(&mut o, "installation_id")?;
    let session_id = id(&mut o, "session_id")?;
    let start = o.text("session_start")?;
    if !session_start(start) {
        return Err(Invalid);
    }
    let batch_seq = o.uint("batch_seq", 0, U32_MAX)?;
    let dropped = o.uint("dropped", 0, U32_MAX)?;
    let device = device(o.required("device")?)?;
    let Val::Arr(items) = o.required("events")? else {
        return Err(Invalid);
    };
    if !(1..=MAX_EVENTS).contains(&items.len()) {
        return Err(Invalid);
    }
    let mut events: Vec<Event> = Vec::with_capacity(items.len());
    for item in items {
        let next = event(item, registry)?;
        if let Some(last) = events.last()
            && (next.seq <= last.seq || next.t < last.t)
        {
            return Err(Invalid);
        }
        events.push(next);
    }
    o.finish()?;
    Ok(Batch {
        release: release_text.to_owned(),
        installation_id,
        session_id,
        session_start: start.to_owned(),
        batch_seq,
        dropped,
        device,
        events,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depth_is_counted_outside_strings() {
        assert!(shallow(br#"{"a":[[[[1]]]]}"#, 5));
        assert!(!shallow(br#"{"a":[[[[[1]]]]]}"#, 5));
        assert!(shallow(br#"{"a":"[[[[[[[[[[\"[[[["}"#, 5));
        assert!(!shallow(&[b'['; 100_000], 5));
    }

    #[test]
    fn identifier_and_format_checks() {
        assert!(uuid_v4("c9a10716-1b78-4cbe-8d1c-b8309e5e7247"));
        for bad in [
            "C9A10716-1B78-4CBE-8D1C-B8309E5E7247",
            "c9a10716-1b78-3cbe-8d1c-b8309e5e7247",
            "c9a10716-1b78-4cbe-cd1c-b8309e5e7247",
            "c9a107161b784cbe8d1cb8309e5e7247",
            "c9a10716-1b78-4cbe-8d1c-b8309e5e724",
            "c9a10716-1b78-4cbe-8d1c-b8309e5e724g",
            "{c9a10716-1b78-4cbe-8d1c-b8309e5e7247}",
        ] {
            assert!(!uuid_v4(bad), "{bad}");
        }
        assert!(release("0.0.0+9d4138ca1f2e"));
        assert!(release("123456789.0.10+000000000000"));
        for bad in [
            "01.0.0+9d4138ca1f2e",
            "0.0.0+9D4138CA1F2E",
            "0.0.0+9d4138ca1f2",
            "0.0+9d4138ca1f2e",
            "0.0.0.0+9d4138ca1f2e",
            "1234567890.0.0+9d4138ca1f2e",
            "0.0.0",
            "0.0.0+9d4138ca1f2e\n",
            "-1.0.0+9d4138ca1f2e",
            "a.0.0+9d4138ca1f2e",
        ] {
            assert!(!release(bad), "{bad}");
        }
        assert!(macos_build("24F74"));
        assert!(macos_build("9A1"));
        assert!(macos_build("21G115a"));
        for bad in [
            "24f74",
            "F74",
            "1234F74",
            "24F",
            "24F123456",
            "24F74ab",
            "24F74A",
            "",
        ] {
            assert!(!macos_build(bad), "{bad}");
        }
    }

    #[test]
    fn session_start_is_a_real_utc_time() {
        assert!(session_start("2026-10-09T14:03:12Z"));
        assert!(session_start("2020-01-01T00:00:00Z"));
        assert!(session_start("2200-12-31T23:59:59Z"));
        assert!(session_start("2024-02-29T00:00:00Z"));
        for bad in [
            "2019-12-31T23:59:59Z",
            "2201-01-01T00:00:00Z",
            "2400-02-29T00:00:00Z",
            "2026-02-29T00:00:00Z",
            "2100-02-29T00:00:00Z",
            "2026-04-31T00:00:00Z",
            "2026-13-01T00:00:00Z",
            "2026-00-01T00:00:00Z",
            "2026-01-00T00:00:00Z",
            "2026-01-01T24:00:00Z",
            "2026-01-01T00:60:00Z",
            "2026-01-01T00:00:60Z",
            "2026-01-01 00:00:00Z",
            "2026-01-01T00:00:00+00:00",
            "2026-01-01T00:00:00z",
            "2026-01-01T00:00:00.000Z",
            "2026-01-01T00:00:00",
            "+026-01-01T00:00:00Z",
        ] {
            assert!(!session_start(bad), "{bad}");
        }
    }

    #[test]
    fn duplicate_keys_and_non_integers_do_not_parse() {
        for bad in [r#"{"a":1,"a":2}"#, r#"{"a":{"b":1,"b":1}}"#] {
            assert!(serde_json::from_str::<Val>(bad).is_err(), "{bad}");
        }
        for number in ["1.0", "-1", "1e2", "18446744073709551616", "-0"] {
            let value: Val = serde_json::from_str(number).unwrap();
            assert!(matches!(value, Val::Number), "{number}");
        }
    }
}
