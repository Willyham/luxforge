//! The typed preview.read seam shared by grid and loupe; batching and cancellation stay with callers.
use super::tasks::{CallError, call_detailed};
use luxforge_core::{
    ClientId, OwnerHandle,
    catalog_types::{PreviewAnswer, PreviewItem, PreviewPriority, PreviewTier},
};
use serde_json::{Value, json};

/// A server refusal or a protocol decoding error, retaining the existing code and message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub(crate) code: String,
    pub(crate) message: String,
}

pub(crate) fn read(
    owner: &OwnerHandle,
    client: ClientId,
    item: &PreviewItem,
    tier: PreviewTier,
    priority: PreviewPriority,
) -> Result<PreviewAnswer, Refusal> {
    decode(call_detailed(
        owner,
        client,
        "preview.read",
        json!({"item": item, "tier": tier, "priority": priority}),
    ))
}

fn decode(answer: Result<Value, CallError>) -> Result<PreviewAnswer, Refusal> {
    answer
        .map_err(|error| Refusal {
            code: error.code,
            message: error.message,
        })
        .and_then(|answer| {
            serde_json::from_value(answer).map_err(|error| Refusal {
                code: "protocol".into(),
                message: error.to_string(),
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_refusal_and_protocol_failure_keep_distinct_codes_and_messages() {
        let error = decode(Err(CallError {
            code: "source-unavailable".into(),
            message: "missing original".into(),
            data: None,
            job_id: None,
        }))
        .unwrap_err();
        assert_eq!(
            error,
            Refusal {
                code: "source-unavailable".into(),
                message: "missing original".into()
            }
        );
        let error = decode(Ok(json!({"unexpected": true}))).unwrap_err();
        assert_eq!(error.code, "protocol");
        assert!(!error.message.is_empty());
    }
}
