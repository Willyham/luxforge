//! Host method parameters, declared once.
//!
//! Every host method's parameters are one struct written with [`host_params!`]. The macro generates
//! the `deny_unknown_fields` struct the request is parsed into and the [`ParamSchema`] `schema.list`
//! publishes, from the same field list: a field typed `Option<T>` is optional and carries the note
//! the schema shows, and every other field is required. A `mutation` field typed [`Mutation`] or
//! [`MutationRequest`] also names the method's [`Envelope`]. The schema therefore cannot list a field
//! the parser refuses, or leave out one it requires.
//!
//! [`Mutation`]: crate::Mutation
//! [`MutationRequest`]: crate::MutationRequest
use crate::{Error, ErrorKind};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

/// Which mutation envelope a method carries in its `mutation` field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Envelope {
    /// None: the method reads, or changes only the caller's own session.
    None,
    /// `{expected_revision, request_id, actor}`: the method changes something that has a revision,
    /// and the store that holds the revision records the request with the change, so it answers a
    /// retry itself, durably.
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
}

/// One method's parameters as `schema.list` publishes them.
#[derive(Debug)]
pub(crate) struct ParamSchema {
    pub required: &'static [&'static str],
    /// Each optional field with the note the schema shows for it.
    pub optional: &'static [(&'static str, &'static str)],
    pub envelope: Envelope,
}

/// A host method's parameter struct: the parser serde derives and the schema generated beside it.
pub(crate) trait HostParams: DeserializeOwned {
    const SCHEMA: ParamSchema;
}

/// Parse one request's parameters, borrowing them rather than cloning the request. Absent params are
/// an empty object; anything but an object is refused, as is any field the struct does not declare.
pub(crate) fn parse<T: DeserializeOwned>(params: &Value) -> Result<T, Error> {
    let parsed = match params {
        Value::Object(_) => T::deserialize(params),
        Value::Null => T::deserialize(&Value::Object(Map::new())),
        _ => {
            return Err(Error::new(
                ErrorKind::Validation,
                "params must be a JSON object",
            ));
        }
    };
    parsed.map_err(|error| Error::new(ErrorKind::Validation, error.to_string()))
}

/// The parameters of a generated method (`edit.*`, `query.*`, `mask.*`, `task.*`) as an owned map:
/// the host takes its envelope fields out with [`take`] and [`take_optional`], and hands what is left
/// to the module's generic parameter check, which owns it.
pub(crate) fn generated(params: &Value) -> Result<Map<String, Value>, Error> {
    match params {
        Value::Object(object) => Ok(object.clone()),
        Value::Null => Ok(Map::new()),
        _ => Err(Error::new(
            ErrorKind::Validation,
            "params must be a JSON object",
        )),
    }
}

/// Take one required envelope field out of a generated method's parameters.
pub(crate) fn take<T: DeserializeOwned>(
    parameters: &mut Map<String, Value>,
    name: &str,
) -> Result<T, Error> {
    take_optional(parameters, name)?
        .ok_or_else(|| Error::new(ErrorKind::Validation, format!("missing field `{name}`")))
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
                .map_err(|error| Error::new(ErrorKind::Validation, format!("{name}: {error}")))
        })
        .transpose()
}

/// Declare one host method's parameters. Written like a struct:
///
/// ```ignore
/// host_params! {
///     /// `history.list`.
///     pub(crate) struct HistoryList {
///         asset_id: AssetId,
///         before_sequence: Option<u64> = "u64",
///         limit: Option<usize> = "1..100",
///     }
/// }
/// ```
///
/// A field typed `Option<T>` is optional and must carry its note after `=`; every other field is
/// required. `mutation: Mutation` and `mutation: MutationRequest` name the envelope. Field attributes
/// pass through to serde.
macro_rules! host_params {
    ($(#[$attr:meta])* $vis:vis struct $name:ident { $($body:tt)* }) => {
        $crate::api::params::host_params!(
            @munch [$(#[$attr])* $vis struct $name] $name [] [] []
            [$crate::api::params::Envelope::None] $($body)*
        );
    };
    // An optional field and its note.
    (@munch [$($head:tt)*] $name:ident [$($fields:tt)*] [$($req:tt)*] [$($opt:tt)*] [$env:expr]
        $(#[$fmeta:meta])* $f:ident : Option<$t:ty> = $note:literal $(, $($rest:tt)*)?) => {
        $crate::api::params::host_params!(
            @munch [$($head)*] $name [$($fields)* $(#[$fmeta])* $f: Option<$t>,] [$($req)*]
            [$($opt)* (stringify!($f), $note),] [$env] $($($rest)*)?
        );
    };
    (@munch [$($head:tt)*] $name:ident [$($fields:tt)*] [$($req:tt)*] [$($opt:tt)*] [$env:expr]
        $(#[$fmeta:meta])* $f:ident : Option<$t:ty> $(, $($rest:tt)*)?) => {
        compile_error!(concat!("optional parameter ", stringify!($f), " needs a note"));
    };
    // The two mutation envelopes.
    (@munch [$($head:tt)*] $name:ident [$($fields:tt)*] [$($req:tt)*] [$($opt:tt)*] [$env:expr]
        mutation : Mutation $(, $($rest:tt)*)?) => {
        $crate::api::params::host_params!(
            @munch [$($head)*] $name [$($fields)* mutation: $crate::Mutation,]
            [$($req)* "mutation",] [$($opt)*] [$crate::api::params::Envelope::Revision]
            $($($rest)*)?
        );
    };
    (@munch [$($head:tt)*] $name:ident [$($fields:tt)*] [$($req:tt)*] [$($opt:tt)*] [$env:expr]
        mutation : MutationRequest $(, $($rest:tt)*)?) => {
        $crate::api::params::host_params!(
            @munch [$($head)*] $name [$($fields)* mutation: $crate::MutationRequest,]
            [$($req)* "mutation",] [$($opt)*] [$crate::api::params::Envelope::Request]
            $($($rest)*)?
        );
    };
    // A required field.
    (@munch [$($head:tt)*] $name:ident [$($fields:tt)*] [$($req:tt)*] [$($opt:tt)*] [$env:expr]
        $(#[$fmeta:meta])* $f:ident : $t:ty $(, $($rest:tt)*)?) => {
        $crate::api::params::host_params!(
            @munch [$($head)*] $name [$($fields)* $(#[$fmeta])* $f: $t,]
            [$($req)* stringify!($f),] [$($opt)*] [$env] $($($rest)*)?
        );
    };
    (@munch [$($head:tt)*] $name:ident [$($fields:tt)*] [$($req:tt)*] [$($opt:tt)*] [$env:expr]) => {
        #[derive(::serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        $($head)* { $($fields)* }
        impl $crate::api::params::HostParams for $name {
            const SCHEMA: $crate::api::params::ParamSchema = $crate::api::params::ParamSchema {
                required: &[$($req)*],
                optional: &[$($opt)*],
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
            asset_id: AssetId,
            mutation: Mutation,
            limit: Option<usize> = "1..100",
        }
    }

    host_params! {
        struct Requested {
            mutation: MutationRequest,
        }
    }

    #[test]
    fn one_declaration_is_both_the_parser_and_the_schema() {
        assert_eq!(Declared::SCHEMA.required, ["asset_id", "mutation"]);
        assert_eq!(Declared::SCHEMA.optional, [("limit", "1..100")]);
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
        for (params, expected) in [
            (json!({"asset_id": asset}), "missing field `mutation`"),
            (json!({"extra": 1}), "unknown field `extra`"),
            (json!([]), "params must be a JSON object"),
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
}
