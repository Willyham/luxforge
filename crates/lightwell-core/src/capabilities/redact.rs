//! Redaction for the one place a secret crosses the API: a `module.settings.set-secret` request.
//! Responses, events, errors, reads and descriptors never carry a secret by construction, so a
//! client that logs, captures or copies requests passes them through here first. See
//! `docs/design/module-capabilities.md#secrets-and-redaction`.
use crate::ApiRequest;
use serde_json::Value;

/// What a redacted secret reads as.
pub const REDACTED: &str = "<redacted>";

/// The method whose `value` parameter is a secret.
const SET_SECRET: &str = super::settings::SET_SECRET;

/// A copy of one request's parameters that is safe to log: the `value` of a
/// `module.settings.set-secret` request is replaced with `"<redacted>"`, whatever its type.
pub fn redact_params(method: &str, params: &Value) -> Value {
    let mut params = params.clone();
    if method == SET_SECRET
        && let Some(value) = params.get_mut("value")
    {
        *value = Value::from(REDACTED);
    }
    params
}

/// A copy of one request that is safe to log, capture as evidence or copy as JSON. The live-session
/// token is dropped too: it authenticates a loopback client, so a copied request must not carry it.
pub fn redact_request(request: &ApiRequest) -> ApiRequest {
    ApiRequest {
        params: redact_params(&request.method, &request.params),
        token: None,
        ..request.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_the_value_of_a_set_secret_request_is_redacted() {
        let request = ApiRequest {
            id: "one".into(),
            method: SET_SECRET.into(),
            params: json!({"module_id": "test.module", "setting": "api-key", "value": "s3cret"}),
            token: Some("live-session-token".into()),
        };
        let redacted = redact_request(&request);
        assert_eq!(
            redacted.params,
            json!({"module_id": "test.module", "setting": "api-key", "value": "<redacted>"})
        );
        assert_eq!(redacted.token, None, "the live-session token is never copied");
        assert_eq!(redacted.id, "one");
        assert_eq!(redacted.method, SET_SECRET);
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
