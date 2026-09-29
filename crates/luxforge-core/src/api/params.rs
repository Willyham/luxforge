//! Host method parameters, declared once.
//!
//! Every host method's parameters are one struct written with [`host_params!`]. The macro generates
//! the `deny_unknown_fields` struct the request is parsed into and the [`ParamSchema`] `schema.list`
//! publishes, from the same field list. Each field is declared with a kind from the one parameter
//! vocabulary module descriptors use ([`ParameterDescriptor`]), written with the constructors in
//! [`kind`]; a field typed `Option<T>` is optional and every other field is required. A `mutation`
//! field typed [`Mutation`] or [`MutationRequest`] names the method's [`Envelope`] instead. The
//! parse checks every field a request names against its declared kind before the struct is
//! deserialized ([`parse`]), so the schema cannot list a field the parser refuses, leave out one it
//! requires, or publish a range it does not enforce.
//!
//! [`Mutation`]: crate::Mutation
//! [`MutationRequest`]: crate::MutationRequest
#[cfg(test)]
use crate::ErrorKind;
use crate::{Error, Mutation, MutationRequest, ParameterDescriptor, ParameterKind, check_value};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Map, Value};

/// Which mutation envelope a method carries in its `mutation` field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Envelope {
    /// None: the method reads, or changes only the caller's own session.
    None,
    /// `{expected_revision, request_id, actor}`: the method changes something that has a revision,
    /// and a stale `expected_revision` is a conflict. The catalog records an asset change's request
    /// with the change and answers its retry itself, durably; the catalog owner answers a settings
    /// write's retry from its request table.
    Revision,
    /// `{request_id, actor}`: nothing the method changes has a revision; the catalog owner answers a
    /// retry from its request table.
    Request,
}

impl Envelope {
    /// The fields of the envelope, as `schema.list` names them.
    pub(crate) fn fields(self) -> Option<&'static [&'static str]> {
        match self {
            Self::None => None,
            Self::Revision => Some(&["expected_revision", "request_id", "actor"]),
            Self::Request => Some(&["request_id", "actor"]),
        }
    }

    /// The envelope's name in a method's `schema.list` entry.
    pub(crate) fn name(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Revision => Some("revision"),
            Self::Request => Some("request"),
        }
    }

    /// Check the envelope a request's `params` carry: `request_id` and `actor` are 1..128
    /// characters. The dispatcher calls this once for every method before any handler runs, so no
    /// handler, service or store checks an envelope of its own. An absent or malformed envelope
    /// passes here: the method's own parse refuses it by name, as it refuses any other field.
    pub(crate) fn check(self, params: &Value) -> Result<(), Error> {
        let Some(field) = params.get("mutation") else {
            return Ok(());
        };
        let checked = match self {
            Self::None => return Ok(()),
            Self::Revision => Mutation::deserialize(field).map(|mutation| mutation.validate()),
            Self::Request => {
                MutationRequest::deserialize(field).map(|mutation| mutation.validate())
            }
        };
        checked.unwrap_or(Ok(()))
    }
}

/// One method's parameters as `schema.list` publishes them.
#[derive(Debug)]
pub(crate) struct ParamSchema {
    /// Every field but the envelope, in declared order, typed with the module parameter
    /// vocabulary; a required field is declared required. Built once, on first use.
    pub parameters: fn() -> &'static [ParameterDescriptor],
    pub envelope: Envelope,
}

/// A host method's parameter struct: the parser serde derives and the schema generated beside it.
pub(crate) trait HostParams: DeserializeOwned {
    const SCHEMA: ParamSchema;
}

/// Parse one request's parameters, borrowing them rather than cloning the request. Absent params are
/// an empty object; anything but an object is refused, as is any field the struct does not declare.
///
/// Each declared field the request names is first checked against its declared kind by the check
/// a module's parameters get ([`check_value`]), so a value outside its published range is refused
/// in the same words whichever method it is sent to: `O(declared fields)`, allocating nothing unless
/// it refuses. `null` is left to the struct, which reads it as an absent optional field and refuses
/// it for a required one, and so is a secret, which is never a plain value to that check: its own
/// type takes it without echoing it, and its setting's limit is checked where it is stored.
pub(crate) fn parse<T: HostParams>(params: &Value) -> Result<T, Error> {
    let parsed = match params {
        Value::Object(object) => {
            for parameter in (T::SCHEMA.parameters)() {
                match object.get(&parameter.name) {
                    None | Some(Value::Null) => {}
                    Some(_) if matches!(parameter.kind, ParameterKind::Secret { .. }) => {}
                    Some(value) => check_value(parameter, value)?,
                }
            }
            T::deserialize(params)
        }
        Value::Null => T::deserialize(&Value::Object(Map::new())),
        _ => {
            return Err(Error::validation("params must be a JSON object"));
        }
    };
    parsed.map_err(|error| Error::validation(error.to_string()))
}

/// The parameters of a generated method (`edit.*`, `query.*`, `mask.*`, `task.*`) as an owned map:
/// the host takes its envelope fields out with [`take`] and [`take_optional`], and hands what is left
/// to the module's generic parameter check, which owns it.
pub(crate) fn generated(params: &Value) -> Result<Map<String, Value>, Error> {
    match params {
        Value::Object(object) => Ok(object.clone()),
        Value::Null => Ok(Map::new()),
        _ => Err(Error::validation("params must be a JSON object")),
    }
}

/// Take one required envelope field out of a generated method's parameters.
pub(crate) fn take<T: DeserializeOwned>(
    parameters: &mut Map<String, Value>,
    name: &str,
) -> Result<T, Error> {
    take_optional(parameters, name)?
        .ok_or_else(|| Error::validation(format!("missing field `{name}`")))
}

/// Take one envelope field the request may omit. A command that requires it says so itself, so the
/// refusal names the command.
pub(crate) fn take_optional<T: DeserializeOwned>(
    parameters: &mut Map<String, Value>,
    name: &str,
) -> Result<Option<T>, Error> {
    parameters
        .remove(name)
        .map(|field| {
            serde_json::from_value(field)
                .map_err(|error| Error::validation(format!("{name}: {error}")))
        })
        .transpose()
}

/// The kinds a host field is declared with, written after its `=` in [`host_params!`]. Each is a
/// [`ParameterDescriptor`] without a name, to which a descriptor's hints chain as they do for a
/// module's parameter (`.notes(..)`, `.unit(..)`, `.default(..)`); the macro names it after its
/// field and marks it required unless the field is an `Option`.
pub(crate) mod kind {
    use crate::{IdentityKind, ParameterDescriptor, ParameterKind};

    /// The longest plain name a host field looks something up by: a module, resource, setting,
    /// profile, grant, capability or action. The `string` kind's own limit.
    pub(crate) const MAX_NAME: usize = 256;
    /// The longest filesystem path a request carries, in bytes: Linux's `PATH_MAX`, which is longer
    /// than macOS's, so no path either platform accepts is refused here.
    pub(crate) const MAX_PATH_BYTES: usize = 4096;

    fn identity(of: IdentityKind) -> ParameterDescriptor {
        ParameterDescriptor::identity("", of)
    }

    pub(crate) fn asset() -> ParameterDescriptor {
        identity(IdentityKind::Asset)
    }

    pub(crate) fn entry() -> ParameterDescriptor {
        identity(IdentityKind::Entry)
    }

    pub(crate) fn draft() -> ParameterDescriptor {
        identity(IdentityKind::Draft)
    }

    pub(crate) fn job() -> ParameterDescriptor {
        identity(IdentityKind::Job)
    }

    pub(crate) fn preset() -> ParameterDescriptor {
        identity(IdentityKind::Preset)
    }

    pub(crate) fn mask() -> ParameterDescriptor {
        identity(IdentityKind::Mask)
    }

    pub(crate) fn component() -> ParameterDescriptor {
        identity(IdentityKind::Component)
    }

    pub(crate) fn artifact() -> ParameterDescriptor {
        ParameterDescriptor::new("", ParameterKind::Artifact)
    }

    pub(crate) fn integer(min: i64, max: i64) -> ParameterDescriptor {
        ParameterDescriptor::integer("", min, max)
    }

    /// An event or history sequence, a cursor a client hands back: any non-negative integer the
    /// catalog can store.
    pub(crate) fn sequence() -> ParameterDescriptor {
        integer(0, i64::MAX)
    }

    pub(crate) fn number(min: f64, max: f64) -> ParameterDescriptor {
        ParameterDescriptor::number("", min, max)
    }

    /// A pixel coordinate of a stage, as a module's point parameter declares one.
    pub(crate) fn pixel_coordinate() -> ParameterDescriptor {
        ParameterDescriptor::pixel_coordinate("")
    }

    pub(crate) fn boolean() -> ParameterDescriptor {
        ParameterDescriptor::boolean("")
    }

    pub(crate) fn enumeration<S: Into<String>>(
        options: impl IntoIterator<Item = S>,
    ) -> ParameterDescriptor {
        ParameterDescriptor::enumeration("", options)
    }

    pub(crate) fn string(max_length: usize) -> ParameterDescriptor {
        ParameterDescriptor::string("", max_length)
    }

    /// A plain name the method looks something up by, which says itself whether it names anything.
    pub(crate) fn name() -> ParameterDescriptor {
        string(MAX_NAME)
    }

    pub(crate) fn text(max_bytes: usize) -> ParameterDescriptor {
        ParameterDescriptor::new("", ParameterKind::Text { max_bytes })
    }

    /// A filesystem path, which may hold any character a file name can.
    pub(crate) fn path() -> ParameterDescriptor {
        text(MAX_PATH_BYTES)
    }

    pub(crate) fn settings() -> ParameterDescriptor {
        ParameterDescriptor::settings("")
    }

    /// A credential as long as any setting may declare one; the setting's own limit is checked
    /// where the value is stored.
    pub(crate) fn secret() -> ParameterDescriptor {
        ParameterDescriptor::secret("", crate::modules::MAX_SECRET_LENGTH)
    }

    /// A structured value whose shape `notes` gives and whose own field type checks it.
    pub(crate) fn json(notes: &str) -> ParameterDescriptor {
        ParameterDescriptor::new("", ParameterKind::Json).notes(notes)
    }

    /// Name a declared kind after its field. [`host_params!`] calls this; nothing else does.
    pub(crate) fn named(
        name: &str,
        required: bool,
        mut parameter: ParameterDescriptor,
    ) -> ParameterDescriptor {
        parameter.name = name.to_owned();
        parameter.required = required;
        parameter
    }
}

/// Declare one host method's parameters. Written like a struct, with each field's kind after `=`:
///
/// ```ignore
/// host_params! {
///     /// `history.list`.
///     pub(crate) struct HistoryList {
///         asset_id: AssetId = asset(),
///         before_sequence: Option<u64> = sequence(),
///         limit: Option<usize> = integer(1, 100).default(50),
///     }
/// }
/// ```
///
/// A kind is one expression of the constructors in [`kind`], with a descriptor's hints chained to
/// it. A field typed `Option<T>` is optional; every other field is required. `mutation: Mutation`
/// and `mutation: MutationRequest` name the envelope and take no kind, because `schema.list`
/// describes each envelope once. Field attributes pass through to serde.
macro_rules! host_params {
    ($(#[$attr:meta])* $vis:vis struct $name:ident { $($body:tt)* }) => {
        $crate::api::params::host_params!(
            @munch [$(#[$attr])* $vis struct $name] $name [] []
            [$crate::api::params::Envelope::None] $($body)*
        );
    };
    // An optional field and its kind.
    (@munch [$($head:tt)*] $name:ident [$($fields:tt)*] [$($params:tt)*] [$env:expr]
        $(#[$fmeta:meta])* $f:ident : Option<$t:ty> = $kind:expr $(, $($rest:tt)*)?) => {
        $crate::api::params::host_params!(
            @munch [$($head)*] $name [$($fields)* $(#[$fmeta])* $f: Option<$t>,]
            [$($params)* (stringify!($f), false, $kind),] [$env] $($($rest)*)?
        );
    };
    // The two mutation envelopes. The dispatcher checks either before the handler runs
    // ([`Envelope::check`]), and a handler reads it only for what it records, such as the actor, so
    // many never read it.
    (@munch [$($head:tt)*] $name:ident [$($fields:tt)*] [$($params:tt)*] [$env:expr]
        mutation : Mutation $(, $($rest:tt)*)?) => {
        $crate::api::params::host_params!(
            @munch [$($head)*] $name [$($fields)* #[allow(dead_code)] mutation: $crate::Mutation,]
            [$($params)*] [$crate::api::params::Envelope::Revision] $($($rest)*)?
        );
    };
    (@munch [$($head:tt)*] $name:ident [$($fields:tt)*] [$($params:tt)*] [$env:expr]
        mutation : MutationRequest $(, $($rest:tt)*)?) => {
        $crate::api::params::host_params!(
            @munch [$($head)*] $name [$($fields)* #[allow(dead_code)] mutation: $crate::MutationRequest,]
            [$($params)*] [$crate::api::params::Envelope::Request] $($($rest)*)?
        );
    };
    // A required field and its kind.
    (@munch [$($head:tt)*] $name:ident [$($fields:tt)*] [$($params:tt)*] [$env:expr]
        $(#[$fmeta:meta])* $f:ident : $t:ty = $kind:expr $(, $($rest:tt)*)?) => {
        $crate::api::params::host_params!(
            @munch [$($head)*] $name [$($fields)* $(#[$fmeta])* $f: $t,]
            [$($params)* (stringify!($f), true, $kind),] [$env] $($($rest)*)?
        );
    };
    (@munch [$($head:tt)*] $name:ident [$($fields:tt)*] [$($params:tt)*] [$env:expr]
        $(#[$fmeta:meta])* $f:ident : $t:ty $(, $($rest:tt)*)?) => {
        compile_error!(concat!("parameter ", stringify!($f), " needs a kind"));
    };
    (@munch [$($head:tt)*] $name:ident [$($fields:tt)*]
        [$(($pname:expr, $required:expr, $pkind:expr),)*] [$env:expr]) => {
        #[derive(::serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        $($head)* { $($fields)* }
        impl $crate::api::params::HostParams for $name {
            const SCHEMA: $crate::api::params::ParamSchema = $crate::api::params::ParamSchema {
                parameters: || {
                    static PARAMETERS: ::std::sync::LazyLock<
                        ::std::vec::Vec<$crate::ParameterDescriptor>,
                    > = ::std::sync::LazyLock::new(|| {
                        #[allow(unused_imports)]
                        use $crate::api::params::kind::*;
                        ::std::vec![$(
                            $crate::api::params::kind::named($pname, $required, $pkind)
                        ),*]
                    });
                    &PARAMETERS
                },
                envelope: $env,
            };
        }
    };
}
pub(crate) use host_params;

host_params! {
    /// A method that takes no parameters: `{}` or no params at all.
    pub(crate) struct NoParams {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AssetId;
    use serde_json::json;

    host_params! {
        struct Declared {
            asset_id: AssetId = asset(),
            mutation: Mutation,
            limit: Option<usize> = integer(1, 100).notes("a page"),
        }
    }

    host_params! {
        struct Requested {
            mutation: MutationRequest,
        }
    }

    #[test]
    fn one_declaration_is_both_the_parser_and_the_schema() {
        assert_eq!(
            (Declared::SCHEMA.parameters)(),
            [
                ParameterDescriptor::identity("asset_id", crate::IdentityKind::Asset)
                    .required(true),
                ParameterDescriptor::integer("limit", 1, 100).notes("a page"),
            ]
        );
        assert!(
            std::ptr::eq(
                (Declared::SCHEMA.parameters)(),
                (Declared::SCHEMA.parameters)()
            ),
            "the parameters are built once"
        );
        assert_eq!(Declared::SCHEMA.envelope, Envelope::Revision);
        assert_eq!(Requested::SCHEMA.envelope, Envelope::Request);
        assert_eq!(NoParams::SCHEMA.envelope, Envelope::None);
        let asset = AssetId::new();
        let parsed: Declared = parse(&json!({
            "asset_id": asset,
            "mutation": {"expected_revision": 3, "request_id": "r", "actor": "a"},
        }))
        .unwrap();
        assert_eq!(parsed.asset_id, asset);
        assert_eq!(parsed.mutation.expected_revision, 3);
        assert_eq!(parsed.limit, None);
        assert_eq!(
            parse::<Declared>(&json!({
                "asset_id": asset,
                "mutation": {"expected_revision": 3, "request_id": "r", "actor": "a"},
                "limit": null,
            }))
            .unwrap()
            .limit,
            None,
            "null is an absent optional field"
        );
        // The declared kinds are checked before the struct is parsed, in the words a module
        // parameter's check uses, and the struct still refuses what no kind covers.
        for (params, expected) in [
            (json!({"asset_id": asset}), "missing field `mutation`"),
            (json!({"extra": 1}), "unknown field `extra`"),
            (json!([]), "params must be a JSON object"),
            (
                json!({"asset_id": "entry-0123456789"}),
                "parameter asset_id must be an asset identity",
            ),
            (
                json!({"asset_id": asset, "limit": 101}),
                "parameter limit must be an integer within 1..=100",
            ),
            (
                json!({"asset_id": asset, "limit": 0}),
                "parameter limit must be an integer within 1..=100",
            ),
            (json!({"asset_id": null}), "invalid type: null"),
        ] {
            let error = parse::<Declared>(&params).map(|_| ()).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Validation);
            assert!(error.detail.contains(expected), "{}", error.detail);
        }
        let requested: Requested =
            parse(&json!({"mutation": {"request_id": "r", "actor": "a"}})).unwrap();
        assert_eq!(requested.mutation.request_id, "r");
        let error = parse::<Requested>(&json!({
            "mutation": {"expected_revision": 1, "request_id": "r", "actor": "a"},
        }))
        .map(|_| ())
        .unwrap_err();
        assert!(
            error.detail.contains("unknown field `expected_revision`"),
            "a request envelope carries no revision: {}",
            error.detail
        );
        parse::<NoParams>(&Value::Null).unwrap();
        parse::<NoParams>(&json!({})).unwrap();
        let error = parse::<NoParams>(&json!({"filter": "x"}))
            .map(|_| ())
            .unwrap_err();
        assert_eq!(error.detail, "unknown field `filter`, there are no fields");
    }

    /// The one envelope check refuses a request identity or actor out of range for either
    /// envelope, and leaves an absent or malformed envelope to the method's own parse, which names
    /// the field.
    #[test]
    fn the_envelope_check_refuses_an_identity_out_of_range_and_leaves_the_shape_to_the_parse() {
        let long = "a".repeat(129);
        for (envelope, revision) in [
            (Envelope::Revision, Some(json!(3))),
            (Envelope::Request, None),
        ] {
            let with = |request_id: &str, actor: &str| {
                let mut mutation = json!({"request_id": request_id, "actor": actor});
                if let Some(revision) = &revision {
                    mutation["expected_revision"] = revision.clone();
                }
                json!({"mutation": mutation, "other": 1})
            };
            envelope.check(&with("r", "a")).unwrap();
            envelope
                .check(&with(&"r".repeat(128), &"a".repeat(128)))
                .unwrap();
            for (params, refusal) in [
                (with("", "a"), "request_id must contain 1..128 characters"),
                (
                    with(&long, "a"),
                    "request_id must contain 1..128 characters",
                ),
                (with("r", ""), "actor must contain 1..128 characters"),
                (with("r", &long), "actor must contain 1..128 characters"),
            ] {
                let error = envelope.check(&params).unwrap_err();
                assert_eq!(
                    (error.kind, error.detail.as_str()),
                    (ErrorKind::Validation, refusal)
                );
            }
            for params in [
                json!({}),
                Value::Null,
                json!({"mutation": null}),
                json!({"mutation": {"request_id": "", "actor": "a", "extra": 1}}),
            ] {
                envelope.check(&params).unwrap();
            }
        }
        // A method without an envelope reads none, so a field of that name is its parse's to refuse.
        Envelope::None
            .check(&json!({"mutation": {"request_id": "", "actor": ""}}))
            .unwrap();
    }
}
