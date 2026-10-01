//! Local searchable-list state. Requests carry the displayed entry, text, page and shared inputs;
//! an answer may update the list only while all of them still match. The answer's optional status
//! vocabulary (cards, notice, report link, whether a search is offered) is parsed here and drawn
//! as it is: the desktop decides nothing about what a choice means.
use luxforge_core::{AssetId, EntryId, ParameterDescriptor, ParameterKind, QueryChoiceControl};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// One choice the answer names outside its rows: what is current, or what the module suggests.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub(crate) struct QueryChoiceCard {
    pub(crate) key: String,
    pub(crate) title: String,
    #[serde(default)]
    pub(crate) subtitle: Option<String>,
    #[serde(default)]
    pub(crate) note: Option<String>,
    /// The suggestion's button label; Apply when absent.
    #[serde(default)]
    pub(crate) label: Option<String>,
    #[serde(default = "eligible")]
    pub(crate) eligible: bool,
    #[serde(default)]
    pub(crate) reasons: Vec<String>,
    /// Values its selection sends beside its key.
    #[serde(default)]
    pub(crate) parameters: Map<String, Value>,
}

fn eligible() -> bool {
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum NoticeLevel {
    Info,
    Warning,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub(crate) struct QueryChoiceNotice {
    pub(crate) level: NoticeLevel,
    pub(crate) text: String,
}

/// A page the person may open in their browser, such as a prefilled issue report.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub(crate) struct QueryChoiceReport {
    pub(crate) label: String,
    pub(crate) url: String,
}

/// The status vocabulary a client draws generically; every field is optional.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
struct StatusVocabulary {
    #[serde(default)]
    current: Option<QueryChoiceCard>,
    #[serde(default)]
    suggestion: Option<QueryChoiceCard>,
    #[serde(default)]
    notice: Option<QueryChoiceNotice>,
    #[serde(default)]
    report: Option<QueryChoiceReport>,
    #[serde(default)]
    search: Option<bool>,
}

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
    #[serde(default)]
    pub(crate) parameters: Map<String, Value>,
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
    /// The choice the answer says is current.
    pub(crate) current: Option<QueryChoiceCard>,
    /// The choice the answer suggests, with its own Apply.
    pub(crate) suggestion: Option<QueryChoiceCard>,
    pub(crate) notice: Option<QueryChoiceNotice>,
    pub(crate) report: Option<QueryChoiceReport>,
    /// Whether the answer offers a search at all.
    pub(crate) search: bool,
    /// View state: the Change disclosure that reveals the search under a card. It sends nothing
    /// and closes with its text.
    pub(crate) changing: bool,
    /// The report page this control last asked the platform to open, recorded as evidence. Tests
    /// and evidence runs record it without opening a browser.
    pub(crate) opened: Option<String>,
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
            current: None,
            suggestion: None,
            notice: None,
            report: None,
            search: true,
            changing: false,
            opened: None,
        }
    }
}

impl QueryChoiceUi {
    pub(crate) fn can_retry(&self) -> bool {
        self.error.is_some() && !self.loading
    }

    /// The search field and its rows are drawn when the answer offers a search: at once when no
    /// card is shown, and under a card only while Change is open.
    pub(crate) fn shows_search(&self) -> bool {
        self.search && (self.changing || (self.current.is_none() && self.suggestion.is_none()))
    }

    /// Shared inputs belong to a selection, so they are drawn only while one can be made: with the
    /// search, or beside a suggestion.
    pub(crate) fn shows_inputs(&self) -> bool {
        self.shows_search() || (self.current.is_none() && self.suggestion.is_some())
    }

    /// Open or close Change. Closing drops the typed search and its rows; it answers whether the
    /// cleared text needs a fresh answer.
    pub(crate) fn set_changing(&mut self, open: bool) -> bool {
        self.changing = open;
        if open || (self.text.is_empty() && self.page == 0) {
            return false;
        }
        self.text.clear();
        self.page = 0;
        self.rows.clear();
        true
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
                let vocabulary = answer.get("status").map_or_else(
                    || Some(StatusVocabulary::default()),
                    |status| serde_json::from_value::<StatusVocabulary>(status.clone()).ok(),
                );
                match (rows.filter(|rows| rows.len() <= 100), vocabulary) {
                    (Some(rows), Some(vocabulary)) => {
                        self.rows = rows;
                        self.pages = answer
                            .get("pages")
                            .and_then(Value::as_u64)
                            .unwrap_or(1)
                            .clamp(1, 100) as u32;
                        self.status = answer.get("status").cloned();
                        self.current = vocabulary.current;
                        self.suggestion = vocabulary.suggestion;
                        self.notice = vocabulary.notice;
                        self.report = vocabulary.report;
                        self.search = vocabulary.search.unwrap_or(true);
                        self.error = None;
                    }
                    _ => self.error = Some("Query returned invalid choices".into()),
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
        Ok(chosen(control, &row.key, &row.parameters, shared))
    }

    /// The suggestion's Apply: its key and parameters over the shared inputs, refused while it is
    /// not eligible or the answer is updating.
    pub(crate) fn suggested(
        &self,
        control: &QueryChoiceControl,
        shared: Map<String, Value>,
    ) -> Result<Map<String, Value>, String> {
        let card = self
            .suggestion
            .as_ref()
            .ok_or_else(|| "Nothing is suggested".to_owned())?;
        if !card.eligible {
            return Err(card.reasons.join(" · "));
        }
        if self.loading {
            return Err("Choices are updating".into());
        }
        Ok(chosen(control, &card.key, &card.parameters, shared))
    }
}

/// A choice's request: the shared inputs, the choice's own parameters over them, and its key.
fn chosen(
    control: &QueryChoiceControl,
    key: &str,
    parameters: &Map<String, Value>,
    shared: Map<String, Value>,
) -> Map<String, Value> {
    let mut request = shared;
    request.extend(parameters.clone());
    request.insert(control.key.clone(), Value::String(key.to_owned()));
    request
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

    fn answered(answer: Value) -> QueryChoiceUi {
        let mut ui = QueryChoiceUi::default();
        let identity = ui.begin(AssetId::new(), EntryId::new(), control().action, Map::new());
        assert!(ui.accept(&identity, Ok(answer)));
        ui
    }

    #[test]
    fn query_choice_status_vocabulary_decides_cards_search_and_inputs() {
        // No card: the search is drawn at once, with its inputs.
        let plain = answered(json!({"rows": []}));
        assert!(plain.shows_search() && plain.shows_inputs());
        // A suggestion: drawn with its inputs, the search behind Change.
        let mut suggested = answered(json!({"rows": [], "status": {
            "suggestion": {"key":"n","title":"Nikon","eligible":true,
                           "parameters":{"assume":true},"note":"Assumed"},
            "notice": {"level":"info","text":"Detected"},
            "report": {"label":"Report","url":"https://example.com/new"}}}));
        assert!(!suggested.shows_search() && suggested.shows_inputs());
        assert_eq!(
            suggested.suggestion.as_ref().unwrap().note.as_deref(),
            Some("Assumed")
        );
        assert_eq!(suggested.notice.as_ref().unwrap().level, NoticeLevel::Info);
        assert_eq!(
            suggested.report.as_ref().unwrap().url,
            "https://example.com/new"
        );
        assert!(!suggested.set_changing(true), "opening Change asks nothing");
        assert!(suggested.shows_search());
        // The current card: no inputs until Change opens the search.
        let mut current = answered(json!({"rows": [], "status": {
            "current": {"key":"n","title":"Nikon"}}}));
        assert!(!current.shows_search() && !current.shows_inputs());
        current.set_changing(true);
        assert!(current.shows_search() && current.shows_inputs());
        current.text = "Nik".into();
        current.rows = vec![QueryChoiceRow {
            key: "n".into(),
            title: "Nikon".into(),
            eligible: true,
            subtitle: None,
            reasons: Vec::new(),
            parameters: Map::new(),
        }];
        assert!(current.set_changing(false), "closing clears typed text");
        assert!(current.text.is_empty() && current.rows.is_empty() && !current.changing);
        // A status that offers no search hides it whatever the cards.
        let refused = answered(json!({"rows": [], "status": {"search": false,
            "notice": {"level":"warning","text":"Not in the database"}}}));
        assert!(!refused.shows_search() && !refused.shows_inputs());
        // A malformed vocabulary is an invalid answer, not a partly drawn one.
        let mut ui = QueryChoiceUi::default();
        let identity = ui.begin(AssetId::new(), EntryId::new(), control().action, Map::new());
        ui.accept(
            &identity,
            Ok(json!({"rows": [], "status": {"notice": {"level":"loud","text":"x"}}})),
        );
        assert_eq!(ui.error.as_deref(), Some("Query returned invalid choices"));
    }

    #[test]
    fn query_choice_apply_and_rows_send_their_own_parameters_over_shared_inputs() {
        let shared = Map::from_iter([("focal".to_owned(), json!(35.0))]);
        let ui = answered(json!({"rows": [
            {"key":"r","title":"Row","eligible":true,"parameters":{"assume":true,"focal":24.0}}
        ], "status": {"suggestion": {"key":"s","title":"Suggested","eligible":true,
                                     "parameters":{"assume":true}}}}));
        assert_eq!(
            ui.suggested(&control(), shared.clone()).unwrap(),
            Map::from_iter([
                ("focal".to_owned(), json!(35.0)),
                ("assume".to_owned(), json!(true)),
                ("key".to_owned(), json!("s"))
            ])
        );
        assert_eq!(
            ui.selection(&control(), "r", shared.clone()).unwrap(),
            Map::from_iter([
                ("focal".to_owned(), json!(24.0)),
                ("assume".to_owned(), json!(true)),
                ("key".to_owned(), json!("r"))
            ])
        );
        let blocked = answered(json!({"rows": [], "status": {"suggestion": {
            "key":"s","title":"Suggested","eligible":false,"reasons":["focal-missing"]}}}));
        assert_eq!(
            blocked.suggested(&control(), shared).unwrap_err(),
            "focal-missing"
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
