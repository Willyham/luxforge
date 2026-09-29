//! Redaction for the one place a secret crosses the API: a `module.settings.set-secret` request.
//! Responses, events, errors, reads and descriptors never carry a secret by construction, so a
//! client that logs, captures or copies requests passes them through here first. See
//! `docs/design/module-capabilities.md#secrets-and-redaction`.
use serde_json::Value;
use std::borrow::Cow;

/// What a redacted secret reads as.
pub const REDACTED: &str = "<redacted>";

/// The method whose `value` parameter is a secret.
const SET_SECRET: &str = super::settings::SET_SECRET;

/// A copy of one request's parameters that is safe to log: the `value` of a
/// `module.settings.set-secret` request is replaced with `"<redacted>"`, whatever its type.
pub fn redact_params(method: &str, params: &Value) -> Value {
    redacted(method, params).into_owned()
}

/// [`redact_params`], copying only the parameters that carry a secret.
pub(crate) fn redacted<'a>(method: &str, params: &'a Value) -> Cow<'a, Value> {
    if method != SET_SECRET || params.get("value").is_none() {
        return Cow::Borrowed(params);
    }
    let mut params = params.clone();
    params["value"] = Value::from(REDACTED);
    Cow::Owned(params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_the_value_of_a_set_secret_request_is_redacted() {
        let params = json!({"module_id": "test.module", "setting": "api-key", "value": "s3cret"});
        assert_eq!(
            redact_params(SET_SECRET, &params),
            json!({"module_id": "test.module", "setting": "api-key", "value": "<redacted>"})
        );
        assert_eq!(
            redact_params(SET_SECRET, &json!({"value": {"nested": "s3cret"}})),
            json!({"value": "<redacted>"}),
            "a malformed value is redacted whatever its type"
        );
        let other = json!({"module_id": "test.module", "values": {"value": "plain"}});
        assert_eq!(redact_params("module.settings.set", &other), other);
        assert_eq!(redact_params(SET_SECRET, &json!(null)), json!(null));
    }
}
