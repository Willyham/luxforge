//! Local searchable-list state. Requests carry the displayed entry, text, page and shared inputs;
//! an answer may update the list only while all of them still match.
use luxforge_core::{AssetId, EntryId, ParameterDescriptor, ParameterKind, QueryChoiceControl};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct QueryChoiceIdentity {
    pub(crate) sequence: u64,
    pub(crate) asset: AssetId,
    pub(crate) entry: EntryId,
    pub(crate) action: String,
    pub(crate) text: String,
    pub(crate) page: u32,
    pub(crate) shared: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub(crate) struct QueryChoiceRow {
    pub(crate) key: String,
    pub(crate) title: String,
    pub(crate) eligible: bool,
    #[serde(default)]
    pub(crate) subtitle: Option<String>,
    #[serde(default)]
    pub(crate) reasons: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct QueryChoiceUi {
    pub(crate) text: String,
    pub(crate) page: u32,
    pub(crate) pages: u32,
    pub(crate) rows: Vec<QueryChoiceRow>,
    pub(crate) loading: bool,
    pub(crate) error: Option<String>,
    pub(crate) request: Option<QueryChoiceIdentity>,
    /// The module's optional status data, kept for its status line without affecting selection.
    pub(crate) status: Option<Value>,
}

impl Default for QueryChoiceUi {
    fn default() -> Self {
        Self {
            text: String::new(),
            page: 0,
            pages: 1,
            rows: Vec::new(),
            loading: false,
            error: None,
            request: None,
            status: None,
        }
    }
}

impl QueryChoiceUi {
    pub(crate) fn can_retry(&self) -> bool {
        self.error.is_some() && !self.loading
    }

    /// A query may narrow its declared shared inputs for this source. Absent hints display all.
    pub(crate) fn shows_shared(&self, name: &str) -> bool {
        self.status
            .as_ref()
            .and_then(|status| status.get("visible_shared"))
            .and_then(Value::as_array)
            .is_none_or(|names| names.iter().any(|value| value.as_str() == Some(name)))
    }

    pub(crate) fn begin(
        &mut self,
        asset: AssetId,
        entry: EntryId,
        action: String,
        shared: Map<String, Value>,
    ) -> QueryChoiceIdentity {
        let identity = QueryChoiceIdentity {
            sequence: self
                .request
                .as_ref()
                .map_or(1, |request| request.sequence.wrapping_add(1)),
            asset,
            entry,
            action,
            text: self.text.clone(),
            page: self.page,
            shared,
        };
        self.request = Some(identity.clone());
        self.loading = true;
        self.rows.clear();
        self.error = None;
        identity
    }

    pub(crate) fn accept(
        &mut self,
        identity: &QueryChoiceIdentity,
        result: Result<Value, String>,
    ) -> bool {
        if self.request.as_ref() != Some(identity)
            || self.text != identity.text
            || self.page != identity.page
        {
            return false;
        }
        self.loading = false;
        match result {
            Ok(answer) => {
                let rows = answer.get("rows").and_then(|value| {
                    serde_json::from_value::<Vec<QueryChoiceRow>>(value.clone()).ok()
                });
                match rows.filter(|rows| rows.len() <= 100) {
                    Some(rows) => {
                        self.rows = rows;
                        self.pages = answer
                            .get("pages")
                            .and_then(Value::as_u64)
                            .unwrap_or(1)
                            .clamp(1, 100) as u32;
                        self.status = answer.get("status").cloned();
                        self.error = None;
                    }
                    None => self.error = Some("Query returned invalid choices".into()),
                }
            }
            Err(error) => self.error = Some(error),
        }
        true
    }

    pub(crate) fn change_page(&mut self, page: u32) -> bool {
        if page >= self.pages || page == self.page {
            return false;
        }
        self.page = page;
        true
    }

    pub(crate) fn selection(
        &self,
        control: &QueryChoiceControl,
        key: &str,
        shared: Map<String, Value>,
    ) -> Result<Map<String, Value>, String> {
        let row = self
            .rows
            .iter()
            .find(|row| row.key == key)
            .ok_or_else(|| "Choice is no longer displayed".to_owned())?;
        if !row.eligible {
            return Err(row.reasons.join(" · "));
        }
        if self.loading {
            return Err("Choices are updating".into());
        }
        let mut parameters = shared;
        parameters.insert(control.key.clone(), Value::String(row.key.clone()));
        Ok(parameters)
    }
}

pub(crate) fn query_parameters(
    control: &QueryChoiceControl,
    identity: &QueryChoiceIdentity,
) -> Map<String, Value> {
    let mut parameters = identity.shared.clone();
    parameters.insert(control.text.clone(), Value::String(identity.text.clone()));
    parameters.insert(control.page.clone(), Value::from(identity.page));
    parameters
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SharedInput {
    pub(crate) parameter: ParameterDescriptor,
    pub(crate) text: String,
}

impl SharedInput {
    pub(crate) fn is_boolean(&self) -> bool {
        matches!(self.parameter.kind, ParameterKind::Boolean)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct QueryChoiceModel {
    pub(crate) control: QueryChoiceControl,
    pub(crate) ui: QueryChoiceUi,
    pub(crate) shared: Vec<SharedInput>,
    pub(crate) enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn control() -> QueryChoiceControl {
        QueryChoiceControl {
            label: "Profiles".into(),
            query: "profiles".into(),
            text: "text".into(),
            page: "page".into(),
            action: "select-profile".into(),
            key: "key".into(),
            shared: vec!["focal".into()],
        }
    }

    #[test]
    fn query_choice_status_can_narrow_declared_shared_inputs() {
        let mut ui = QueryChoiceUi::default();
        assert!(ui.shows_shared("assume-uncorrected"));
        ui.status = Some(json!({"visible_shared":["focal"]}));
        assert!(ui.shows_shared("focal"));
        assert!(!ui.shows_shared("assume-uncorrected"));
        ui.status = Some(json!({"visible_shared":["focal", "assume-uncorrected"]}));
        assert!(ui.shows_shared("assume-uncorrected"));
    }

    #[test]
    fn query_choice_drops_answers_for_stale_text() {
        let mut ui = QueryChoiceUi::default();
        let asset = AssetId::new();
        let entry = EntryId::new();
        let first = ui.begin(asset.clone(), entry.clone(), control().action, Map::new());
        ui.text = "Nikon".into();
        let second = ui.begin(asset, entry, control().action, Map::new());
        assert!(!ui.accept(&first, Ok(json!({"rows": []}))));
        assert!(ui.loading);
        assert!(ui.accept(
            &second,
            Ok(json!({"rows": [{"key":"n", "title":"Nikon", "eligible":true}]}))
        ));
        assert_eq!(ui.rows[0].title, "Nikon");
    }

    #[test]
    fn query_choice_same_input_retry_recovers_and_rejects_pre_retry_answers() {
        let mut ui = QueryChoiceUi {
            text: "Nikon".into(),
            page: 2,
            ..QueryChoiceUi::default()
        };
        let asset = AssetId::new();
        let entry = EntryId::new();
        let shared = Map::from_iter([("focal".into(), json!(35.0))]);
        let first = ui.begin(
            asset.clone(),
            entry.clone(),
            control().action,
            shared.clone(),
        );
        assert!(ui.accept(&first, Err("not-ready: source preparation required".into())));
        assert!(ui.can_retry());
        let retry = ui.begin(asset, entry, control().action, shared);
        assert_eq!(retry.sequence, first.sequence + 1);
        let mut same_input = retry.clone();
        same_input.sequence = first.sequence;
        assert_eq!(same_input, first);
        assert!(!ui.can_retry(), "the retry is loading");
        assert!(!ui.accept(
            &first,
            Ok(json!({"rows":[{"key":"old","title":"Old","eligible":true}]}))
        ));
        assert!(ui.loading && ui.rows.is_empty());
        assert!(ui.accept(
            &retry,
            Ok(json!({"rows":[{"key":"new","title":"Ready","eligible":true}]}))
        ));
        assert!(!ui.loading && ui.error.is_none() && !ui.can_retry());
        assert_eq!(ui.rows[0].key, "new");
        assert!(!ui.accept(&first, Err("not-ready: old failure".into())));
        assert!(ui.error.is_none());
        assert_eq!(ui.rows[0].key, "new");
    }

    #[test]
    fn query_choice_pages_next_and_previous() {
        let mut ui = QueryChoiceUi {
            pages: 3,
            ..QueryChoiceUi::default()
        };
        assert!(ui.change_page(1));
        assert_eq!(ui.page, 1);
        assert!(ui.change_page(2));
        assert!(!ui.change_page(3));
        assert!(ui.change_page(1));
        assert!(ui.change_page(0));
        assert!(!ui.change_page(0));
    }

    #[test]
    fn query_choice_sends_shared_parameters_to_query_and_action() {
        let mut ui = QueryChoiceUi::default();
        let shared = Map::from_iter([("focal".to_owned(), json!(35.0))]);
        let identity = ui.begin(
            AssetId::new(),
            EntryId::new(),
            control().action,
            shared.clone(),
        );
        assert_eq!(query_parameters(&control(), &identity)["focal"], 35.0);
        ui.accept(
            &identity,
            Ok(json!({"rows": [{"key":"n", "title":"Nikon", "eligible":true}]})),
        );
        assert_eq!(
            ui.selection(&control(), "n", shared).unwrap(),
            Map::from_iter([
                ("focal".to_owned(), json!(35.0)),
                ("key".to_owned(), json!("n"))
            ])
        );
    }

    #[test]
    fn query_choice_ineligible_row_shows_reasons_and_does_not_submit() {
        let mut ui = QueryChoiceUi::default();
        let identity = ui.begin(AssetId::new(), EntryId::new(), control().action, Map::new());
        ui.accept(&identity, Ok(json!({"rows": [{"key":"n", "title":"Nikon", "eligible":false, "reasons":["Already corrected"]}]})));
        assert_eq!(ui.rows[0].reasons, ["Already corrected"]);
        assert_eq!(
            ui.selection(&control(), "n", Map::new()).unwrap_err(),
            "Already corrected"
        );
    }
}
