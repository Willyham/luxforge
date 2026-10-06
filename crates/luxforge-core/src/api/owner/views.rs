//! Browse views on the owner: each client's one view — its evaluated item list
//! ([`ViewItem`]s, 16 bytes each) with its layout and selection, kept here and never in the
//! session, whose `browse` part ([`BrowseSession`]) `session.state` reports — dropped when the
//! client disconnects; the event cache; and the handlers of `event.list`, `browse.view`,
//! `browse.rows`, `browse.facets` and `browse.select` (`crate::catalog_types::api`). Views are
//! evaluated on the owner by `crate::browse`, which reads compact columns and does bookkeeping
//! only, so nothing here posts messages.
//!
//! **Staleness.** A view is stamped with the catalog's latest library change and the index's
//! revision. It is marked stale — here and in its session — after every change the owner records
//! ([`changed`]), and when its client reads rows or selects; its session also finds it stale
//! whenever the session is read (`session.state` and every answer that carries the session). A
//! stale view still answers rows for the items it holds; [`ViewsLane::selected`] refuses it, so a
//! library change never acts on a selection its client has not seen since a later change.
//! [`current_items`] instead evaluates a stale view again in place from its stored query, since
//! what it answers — every item of the view, whose picks a Develop takes — names no positions the
//! person saw; and [`freshen_selection`], before any request that targets the selection, does the
//! same and keeps the view fresh only when its selection still names the same items.

use super::{Call, ClientId, Owner};
use crate::{
    EditorService, Error,
    api::ClientSession,
    api::methods::session_value,
    browse::{self, Context, EventCache, Probe, SelectRequest, View},
    catalog_types::{
        BrowseSession, MAX_VIEW_ITEMS, ViewItem, ViewQuery, ViewRows, ViewSummary,
        api::{BrowseFacets, BrowseRows, BrowseSelect, BrowseView, EventListParams},
    },
};
use serde_json::Value;
use std::collections::HashMap;

/// Browse views' state on the owner: every client's view and the events of the index.
#[derive(Default)]
pub(super) struct ViewsLane {
    views: HashMap<ClientId, View>,
    events: EventCache,
}

impl ViewsLane {
    pub(super) fn disconnect(&mut self, client: ClientId) {
        self.views.remove(&client);
    }

    /// The items selected in `client`'s current view, in view order: what a library method's
    /// `{kind: selection}` targets name. Refused with `validation` when the client holds no view,
    /// and with `conflict` when its view is stale.
    pub(super) fn selected(&self, client: ClientId) -> Result<Vec<ViewItem>, Error> {
        Ok(browse::selected_items(self.fresh(client)?))
    }

    /// `client`'s view, refused with `validation` when it holds none and with `conflict` when a
    /// later library or index change left it stale, so a library change never acts on a view its
    /// client has not seen since.
    fn fresh(&self, client: ClientId) -> Result<&View, Error> {
        let view = self.views.get(&client).ok_or_else(no_view)?;
        if view.stale {
            return Err(Error::conflict(
                "the view is stale: a later library or index change exists; evaluate it again",
            ));
        }
        Ok(view)
    }
}

fn no_view() -> Error {
    Error::validation("this client holds no view; evaluate one with browse.view")
}

/// Ask the preview lane for a view's missing grid previews, as one background job per client that
/// replaces the client's previous one. The view is answered whatever the lane says: previews are a
/// cache, and a view whose background job was refused (a view past the lane's queue bound, or an
/// index read that failed) still browses, its visible cells read through `preview.read` at their
/// own priority as the client asks for them.
fn want_view(owner: &mut Owner, client: ClientId, items: &[ViewItem]) {
    let _ = super::previews::want_view_items(owner, client, items);
}

/// The owner recorded a change: mark every view the catalog or the index has moved on from stale,
/// reading the current library change and index revision once. Nothing is read while no client
/// holds a fresh view.
pub(super) fn changed(owner: &mut Owner) {
    let Owner {
        service,
        sessions,
        catalog,
        ..
    } = owner;
    if catalog.views.views.values().all(|view| view.stale) {
        return;
    }
    // A failed read leaves every view as it was; the next read of a session or view retries.
    let Ok(current) = browse::current_stamp(service) else {
        return;
    };
    for (client, view) in &mut catalog.views.views {
        let Some(session) = sessions.get_mut(client) else {
            continue;
        };
        if session.browse.evaluated_at != Some(current) {
            view.stale = true;
            session.browse.stale = true;
        }
    }
}

/// Photographs left the catalog outside the journal (`catalog.empty-removed`), which moves
/// neither the library sequence nor the index revision a view is stamped with, so every view is
/// marked stale here: none acts on or shows a deleted photograph until it is evaluated again.
pub(super) fn photographs_deleted(owner: &mut Owner) {
    let Owner {
        sessions, catalog, ..
    } = owner;
    for (client, view) in &mut catalog.views.views {
        view.stale = true;
        if let Some(session) = sessions.get_mut(client) {
            session.browse.stale = true;
        }
    }
}

/// `client`'s view and session, the view marked stale first when the catalog or the index has
/// moved on.
fn current_view<'a>(
    service: &EditorService,
    sessions: &'a mut HashMap<ClientId, ClientSession>,
    lane: &'a mut ViewsLane,
    client: ClientId,
) -> Result<(&'a mut View, &'a mut ClientSession), Error> {
    let view = lane.views.get_mut(&client).ok_or_else(no_view)?;
    let session = sessions.entry(client).or_default();
    if browse::refresh_stale(service, &mut session.browse)? {
        view.stale = true;
    }
    Ok((view, session))
}

/// Refuse a request naming a revision other than the view's.
fn check_revision(view: &View, revision: Option<u64>) -> Result<(), Error> {
    match revision {
        Some(revision) if revision != view.revision => Err(Error::conflict(format!(
            "the view is at revision {}, not {revision}",
            view.revision
        ))),
        _ => Ok(()),
    }
}

fn value(value: impl serde::Serialize) -> Result<Value, Error> {
    serde_json::to_value(value).map_err(|error| Error::internal(error.to_string()))
}

/// `event.list`.
pub(in crate::api) fn event_list(
    owner: &mut Owner,
    _: &Call<'_>,
    p: EventListParams,
) -> Result<Value, Error> {
    value(browse::event_list(
        &owner.service,
        &mut owner.catalog.views.events,
        p.month,
        p.query.as_deref(),
    )?)
}

/// Every item of `client`'s current view, in view order: what `pick.plan` and `pick.develop` take
/// by default, the picks among them. A view the catalog or the index has moved on from is evaluated
/// again first, in place, from the query it was evaluated with — its revision moves on and its
/// selection is carried over by item, as any evaluation does — so a Develop takes the picks as they
/// are. Refused with `validation` when the client holds no view.
pub(super) fn current_items(owner: &mut Owner, client: ClientId) -> Result<Vec<ViewItem>, Error> {
    let stale = {
        let Owner {
            service,
            sessions,
            catalog,
            ..
        } = &mut *owner;
        current_view(service, sessions, &mut catalog.views, client)?
            .0
            .stale
    };
    if stale {
        let query = owner
            .sessions
            .get(&client)
            .and_then(|session| session.browse.query.clone())
            .ok_or_else(no_view)?;
        evaluate_into(owner, client, query)?;
    }
    Ok(owner
        .catalog
        .views
        .views
        .get(&client)
        .ok_or_else(no_view)?
        .items
        .clone())
}

/// Before a request that targets `client`'s selection: a view the catalog or the index has moved on
/// from is evaluated again in place from its query, and stays fresh only when the selection it
/// carries over names exactly the items the stale one did, in order — grid previews landing move
/// the index's revision without changing what anyone selected. Otherwise it is left stale, so the
/// selection seam refuses the request: a selection names positions the person saw. Nothing happens
/// without a view, or to a fresh one.
pub(super) fn freshen_selection(owner: &mut Owner, client: ClientId) -> Result<(), Error> {
    let (stale, before) = {
        let Owner {
            service,
            sessions,
            catalog,
            ..
        } = &mut *owner;
        let Ok((view, _)) = current_view(service, sessions, &mut catalog.views, client) else {
            return Ok(());
        };
        (view.stale, browse::selected_items(view))
    };
    if !stale {
        return Ok(());
    }
    let Some(query) = owner
        .sessions
        .get(&client)
        .and_then(|session| session.browse.query.clone())
    else {
        return Ok(());
    };
    evaluate_into(owner, client, query)?;
    let Owner {
        sessions, catalog, ..
    } = &mut *owner;
    if let Some(view) = catalog.views.views.get_mut(&client)
        && browse::selected_items(view) != before
    {
        view.stale = true;
        if let Some(session) = sessions.get_mut(&client) {
            session.browse.stale = true;
        }
    }
    Ok(())
}

/// `browse.view`: evaluate the query into the caller's one view, carrying its selection over by
/// item, and answer the summary.
pub(in crate::api) fn browse_view(
    owner: &mut Owner,
    call: &Call<'_>,
    p: BrowseView,
) -> Result<Value, Error> {
    let query = ViewQuery {
        source: p.source,
        filter: p.filter.unwrap_or_default(),
        sort: p.sort.unwrap_or_default(),
        grouping: p.grouping.unwrap_or_default(),
        thresholds: p.thresholds.unwrap_or_default(),
    };
    value(evaluate_into(owner, call.client, query)?)
}

/// Evaluate `query` into `client`'s one view, carrying its selection over by item, and answer the
/// summary.
fn evaluate_into(
    owner: &mut Owner,
    client: ClientId,
    query: ViewQuery,
) -> Result<ViewSummary, Error> {
    let evaluation = browse::evaluate(
        Context {
            service: &owner.service,
            events: &mut owner.catalog.views.events,
            probe: Probe::Previews,
            limit: MAX_VIEW_ITEMS,
        },
        &query,
    )?;
    let selection = owner
        .catalog
        .views
        .views
        .remove(&client)
        .map(|old| browse::carry_over(&old.items, &old.selection, &evaluation.items))
        .unwrap_or_default();
    let session = owner.sessions.entry(client).or_default();
    let revision = session.browse.revision.saturating_add(1);
    let count = evaluation.items.len() as u32;
    let over_files = query.source.over_files();
    *session.browse = BrowseSession {
        query: Some(query.clone()),
        revision,
        count,
        stale: false,
        selection: selection.clone(),
        evaluated_at: Some(evaluation.stamp),
    };
    session.touch();
    let summary = ViewSummary {
        revision,
        query,
        count,
        picked: evaluation.picked,
        in_catalog: evaluation.in_catalog,
        unavailable: evaluation.unavailable,
        groups: evaluation.layout.clone(),
        library_sequence: evaluation.stamp.library_sequence,
        index_revision: evaluation.stamp.index_revision,
    };
    want_view(owner, client, &evaluation.items);
    owner.catalog.views.views.insert(
        client,
        View {
            revision,
            over_files,
            items: evaluation.items,
            layout: evaluation.layout,
            selection,
            stale: false,
        },
    );
    Ok(summary)
}

/// `browse.rows`.
pub(in crate::api) fn browse_rows(
    owner: &mut Owner,
    call: &Call<'_>,
    p: BrowseRows,
) -> Result<Value, Error> {
    let Owner {
        service,
        sessions,
        catalog,
        ..
    } = owner;
    let previews = &catalog.previews;
    let (view, _) = current_view(service, sessions, &mut catalog.views, call.client)?;
    check_revision(view, p.revision)?;
    value(ViewRows {
        revision: view.revision,
        from: p.from,
        rows: browse::rows(service, view, p.from, p.count, &|items| {
            previews.view_grid_states(service, items)
        })?,
    })
}

/// `browse.facets`.
pub(in crate::api) fn browse_facets(
    owner: &mut Owner,
    _: &Call<'_>,
    p: BrowseFacets,
) -> Result<Value, Error> {
    value(browse::facets(
        Context {
            service: &owner.service,
            events: &mut owner.catalog.views.events,
            probe: Probe::Previews,
            limit: MAX_VIEW_ITEMS,
        },
        &p.source,
        &p.filter.unwrap_or_default(),
        &p.facets,
    )?)
}

/// `browse.select`: change the caller's selection and answer its session.
pub(in crate::api) fn browse_select(
    owner: &mut Owner,
    call: &Call<'_>,
    p: BrowseSelect,
) -> Result<Value, Error> {
    let Owner {
        service,
        sessions,
        catalog,
        ..
    } = owner;
    let (view, session) = current_view(service, sessions, &mut catalog.views, call.client)?;
    check_revision(view, p.revision)?;
    let mut selection = view.selection.clone();
    browse::select(
        service,
        view,
        &mut selection,
        SelectRequest {
            mode: p.mode.unwrap_or_default(),
            items: p.items,
            range: p.range,
            all: p.all,
            active: p.active,
        },
    )?;
    if selection != view.selection {
        session.browse.selection = selection.clone();
        view.selection = selection;
        session.touch();
    }
    session_value(service, session)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ApiRequest, OwnerHandle,
        browse::testing::{self, Fixture},
        catalog_types::{PositionRange, ViewSelection},
    };
    use serde_json::json;

    fn call(
        owner: &OwnerHandle,
        client: ClientId,
        method: &str,
        params: Value,
    ) -> Result<Value, (String, String)> {
        let response = owner
            .call(
                client,
                ApiRequest {
                    id: method.into(),
                    method: method.into(),
                    params,
                    token: None,
                },
            )
            .expect("the owner answered");
        match response.error {
            None => Ok(response.result.expect("a result")),
            Some(error) => Err((error.code, error.message)),
        }
    }

    fn ok(owner: &OwnerHandle, client: ClientId, method: &str, params: Value) -> Value {
        call(owner, client, method, params).unwrap_or_else(|error| panic!("{method}: {error:?}"))
    }

    fn code(owner: &OwnerHandle, client: ClientId, method: &str, params: Value) -> String {
        match call(owner, client, method, params) {
            Ok(value) => panic!("{method} answered {value}"),
            Err((code, _)) => code,
        }
    }

    fn index(fx: &Fixture) -> rusqlite::Connection {
        rusqlite::Connection::open(crate::index::index_dir(&fx.catalog).join(crate::INDEX_FILE))
            .unwrap()
    }

    fn positions(ranges: &Value) -> Vec<u64> {
        ranges
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|range| {
                let start = range["start"].as_u64().unwrap();
                start..start + range["len"].as_u64().unwrap()
            })
            .collect()
    }

    /// The five methods through the owner, as the desktop and an agent send them: events, a view
    /// and its windows, the selection reported in `session.state` and carried over by item, a
    /// revision refused, staleness after an index revision, and a view dropped on disconnect.
    #[test]
    fn browse_methods_answer_through_the_owner() {
        let fx = testing::fixture("owner");
        let (owner, join) = OwnerHandle::start(&fx.catalog).unwrap();
        let client = owner.register();
        let other = owner.register();

        let schema = ok(&owner, client, "schema.list", json!({}));
        for method in [
            "event.list",
            "browse.view",
            "browse.rows",
            "browse.facets",
            "browse.select",
        ] {
            assert!(schema["methods"][method].is_object(), "{method} is listed");
            assert_eq!(
                schema["methods"][method]["mutates"],
                json!(false),
                "{method}"
            );
        }

        let events = ok(&owner, client, "event.list", json!({"month": "2026-09"}));
        assert!(events["events"][0]["count"].as_u64().unwrap() > 0);
        assert_eq!(events["months"][0]["month"], "2026-09");
        assert_eq!(
            code(&owner, client, "event.list", json!({"month": "2026-13"})),
            "validation"
        );

        // Nothing to read or select before a view.
        assert_eq!(
            code(
                &owner,
                client,
                "browse.rows",
                json!({"from": 0, "count": 5})
            ),
            "validation"
        );
        assert_eq!(
            code(&owner, client, "browse.select", json!({"all": true})),
            "validation"
        );

        let card = json!({"kind": "card", "volume_id": testing::volume("card")});
        let summary = ok(
            &owner,
            client,
            "browse.view",
            json!({"source": card, "sort": {"key": "file-name"}}),
        );
        assert_eq!(summary["revision"], 1);
        assert_eq!(summary["count"], 15);
        assert_eq!(summary["picked"], 2);
        assert_eq!(summary["in_catalog"], 1);
        assert_eq!(
            summary["query"]["grouping"], "day-camera-moment",
            "defaults filled in"
        );
        assert_eq!(
            summary["groups"],
            json!({"days": [], "cameras": [], "moments": []}),
            "a file-name sort is ungrouped"
        );
        assert_eq!(summary["library_sequence"], 0);
        assert_eq!(summary["index_revision"], 0);
        let session = ok(&owner, client, "session.state", json!({}));
        assert_eq!(session["browse"]["revision"], 1);
        assert_eq!(session["browse"]["count"], 15);
        assert_eq!(session["browse"]["stale"], false);
        assert_eq!(session["browse"]["query"]["source"]["kind"], "card");
        assert!(
            session["browse"].get("evaluated_at").is_none(),
            "the stamp is never serialized"
        );

        let window = ok(
            &owner,
            client,
            "browse.rows",
            json!({"from": 13, "count": 5, "revision": 1}),
        );
        assert_eq!(window["revision"], 1);
        assert_eq!(window["rows"].as_array().unwrap().len(), 2);
        assert_eq!(window["rows"][0]["position"], 13);
        assert_eq!(window["rows"][1]["file_name"], "DSC_0102.NEF");
        assert_eq!(
            code(
                &owner,
                client,
                "browse.rows",
                json!({"from": 0, "count": 5, "revision": 2})
            ),
            "conflict"
        );
        assert_eq!(
            code(
                &owner,
                client,
                "browse.rows",
                json!({"from": 0, "count": 1001})
            ),
            "validation"
        );

        // Select a range with the active item, then add an item by its identity.
        let dsc_0102 = fx.file(&format!("{}/DSC_0102.NEF", testing::CARD_2)).id;
        let answered = ok(
            &owner,
            client,
            "browse.select",
            json!({"range": {"start": 2, "len": 3}, "active": 3, "revision": 1}),
        );
        assert_eq!(
            answered["browse"]["selection"],
            json!({"count": 3, "ranges": [{"start": 2, "len": 3}], "active": 3})
        );
        let answered = ok(
            &owner,
            client,
            "browse.select",
            json!({"mode": "add", "items": [{"file_id": dsc_0102.0}]}),
        );
        assert_eq!(answered["browse"]["selection"]["count"], 4);
        assert_eq!(
            code(
                &owner,
                client,
                "browse.select",
                json!({"all": true, "revision": 7})
            ),
            "conflict"
        );
        assert_eq!(
            code(&owner, client, "browse.select", json!({"mode": "sideways"})),
            "validation"
        );
        let session = ok(&owner, client, "session.state", json!({}));
        assert_eq!(
            session["browse"]["selection"]["ranges"],
            json!([{"start": 2, "len": 3}, {"start": 14, "len": 1}])
        );

        // Evaluated again in another order, the selection follows its items.
        let rows = ok(
            &owner,
            client,
            "browse.rows",
            json!({"from": 0, "count": 15}),
        );
        let selected: Vec<Value> = rows["rows"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| [2, 3, 4, 14].contains(&row["position"].as_u64().unwrap()))
            .map(|row| row["file_id"].clone())
            .collect();
        let resorted = ok(
            &owner,
            client,
            "browse.view",
            json!({"source": card, "sort": {"key": "file-name", "descending": true}}),
        );
        assert_eq!(resorted["revision"], 2);
        let session = ok(&owner, client, "session.state", json!({}));
        assert_eq!(session["browse"]["selection"]["count"], 4);
        let rows = ok(
            &owner,
            client,
            "browse.rows",
            json!({"from": 0, "count": 15, "revision": 2}),
        );
        let now_at: Vec<u64> = rows["rows"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| selected.contains(&row["file_id"]))
            .map(|row| row["position"].as_u64().unwrap())
            .collect();
        assert_eq!(positions(&session["browse"]["selection"]["ranges"]), now_at);

        // Facets need no view.
        let facets = ok(
            &owner,
            other,
            "browse.facets",
            json!({"source": card, "facets": ["pick", "kind"]}),
        );
        assert_eq!(
            facets["counts"]["pick"][1],
            json!({"value": "picked", "label": "Picked", "count": 2})
        );
        assert_eq!(
            code(
                &owner,
                other,
                "browse.facets",
                json!({"source": {"kind": "all-photographs"}, "facets": ["pick"]})
            ),
            "validation"
        );

        // Another client's view is its own.
        ok(
            &owner,
            other,
            "browse.view",
            json!({"source": {"kind": "all-photographs"}}),
        );
        assert_eq!(
            ok(&owner, other, "session.state", json!({}))["browse"]["count"],
            14
        );
        assert_eq!(
            ok(&owner, client, "session.state", json!({}))["browse"]["count"],
            15
        );

        // An index revision makes every view stale; a stale view still answers its rows.
        testing::set_index_revision(&index(&fx), 3);
        assert_eq!(
            ok(&owner, client, "session.state", json!({}))["browse"]["stale"],
            true
        );
        let stale_rows = ok(
            &owner,
            client,
            "browse.rows",
            json!({"from": 0, "count": 2}),
        );
        assert_eq!(stale_rows["rows"].as_array().unwrap().len(), 2);
        let fresh = ok(&owner, client, "browse.view", json!({"source": card}));
        assert_eq!(fresh["index_revision"], 3);
        assert_eq!(
            ok(&owner, client, "session.state", json!({}))["browse"]["stale"],
            false
        );

        // A disconnected client's view is gone with its session.
        owner.disconnect(other);
        let again = owner.register();
        assert_eq!(
            code(&owner, again, "browse.rows", json!({"from": 0, "count": 1})),
            "validation"
        );
        assert_eq!(
            ok(&owner, again, "session.state", json!({}))["browse"]["revision"],
            0
        );

        // Queries are validated.
        for bad in [
            json!({"source": card, "filter": {"edited": true}}),
            json!({"source": {"kind": "all-photographs"}, "thresholds": {"run_gap_ms": 0}}),
            json!({"source": {"kind": "all-photographs"}, "grouping": "week"}),
            json!({"source": {"kind": "folder", "path": "relative"}}),
        ] {
            assert_eq!(
                code(&owner, client, "browse.view", bad.clone()),
                "validation",
                "{bad}"
            );
        }
        owner.stop();
        join.join().unwrap();
    }

    /// A Develop's default targets are the view as it is: a view an index revision left stale is
    /// evaluated again in place from its query for `pick.plan` and `pick.develop` — its revision
    /// moves on and it is fresh — rather than refused, while the selection seam still refuses it.
    #[test]
    fn browse_a_stale_view_is_read_again_for_a_develop() {
        let fx = testing::fixture("owner-develop-stale");
        let (owner, join) = OwnerHandle::start(&fx.catalog).unwrap();
        let client = owner.register();
        let card = json!({"kind": "card", "volume_id": testing::volume("card")});
        let summary = ok(&owner, client, "browse.view", json!({ "source": card }));
        let revision = summary["revision"].as_u64().unwrap();
        testing::set_index_revision(&index(&fx), 5);
        assert_eq!(
            ok(&owner, client, "session.state", json!({}))["browse"]["stale"],
            true
        );
        ok(&owner, client, "pick.plan", json!({}));
        let session = ok(&owner, client, "session.state", json!({}));
        assert_eq!(session["browse"]["stale"], false);
        assert_eq!(session["browse"]["revision"], revision + 1);
        assert_eq!(session["browse"]["query"]["source"], card);
        // Stale again: the Develop reads it again too, and starts rather than refusing.
        testing::set_index_revision(&index(&fx), 6);
        let started = ok(
            &owner,
            client,
            "pick.develop",
            json!({"into": [], "mutation": {"request_id": "develop", "actor": "test"}}),
        );
        assert!(started["job_id"].is_string(), "{started}");
        assert_eq!(
            ok(&owner, client, "session.state", json!({}))["browse"]["revision"],
            revision + 2
        );
        owner.stop();
        join.join().unwrap();
    }

    /// A selection the index's revision alone left stale — grid previews landing — still names the
    /// items the person saw, so a request on it is answered, its view evaluated again first.
    #[test]
    fn browse_a_selection_the_index_alone_left_stale_is_acted_on() {
        let fx = testing::fixture("owner-selection-stale");
        let (owner, join) = OwnerHandle::start(&fx.catalog).unwrap();
        let client = owner.register();
        let card = json!({"kind": "card", "volume_id": testing::volume("card")});
        let summary = ok(&owner, client, "browse.view", json!({ "source": card }));
        let revision = summary["revision"].as_u64().unwrap();
        ok(
            &owner,
            client,
            "browse.select",
            json!({"range": {"start": 0, "len": 1}, "active": 0}),
        );
        testing::set_index_revision(&index(&fx), 7);
        assert_eq!(
            ok(&owner, client, "session.state", json!({}))["browse"]["stale"],
            true
        );
        let answer = ok(
            &owner,
            client,
            "pick.set",
            json!({
                "targets": {"kind": "selection"},
                "picked": true,
                "mutation": {"request_id": "pick", "actor": "test"},
            }),
        );
        assert!(answer["change"].is_u64(), "{answer}");
        let session = ok(&owner, client, "session.state", json!({}));
        assert_eq!(session["browse"]["revision"], revision + 1);
        assert_eq!(session["browse"]["selection"]["active"], 0);
        owner.stop();
        join.join().unwrap();
    }

    /// A change the owner records marks every view it left behind stale, so the selection seam
    /// refuses it before the client reads its session again.
    #[test]
    fn browse_a_recorded_change_marks_views_stale() {
        let fx = testing::fixture("owner-changed");
        let (owner, join) = OwnerHandle::start(&fx.catalog).unwrap();
        let client = owner.register();
        ok(
            &owner,
            client,
            "browse.view",
            json!({"source": {"kind": "all-photographs"}}),
        );
        testing::set_index_revision(&index(&fx), 1);
        // Any recorded change runs the check; a preset saved to the library records one.
        ok(
            &owner,
            client,
            "preset.create",
            json!({
                "name": "Flat",
                "settings": {"set-basic": {"contrast": -10}},
                "mutation": {"request_id": "preset", "actor": "test"},
            }),
        );
        let session = ok(&owner, client, "session.state", json!({}));
        assert_eq!(session["browse"]["stale"], true);
        owner.stop();
        join.join().unwrap();
    }

    /// The seam the library's selection targets call: the selected items, in view order, refused
    /// without a view and when the view is stale.
    #[test]
    fn browse_the_selected_items_are_refused_when_stale() {
        let client = ClientId::testing(7);
        let mut lane = ViewsLane::default();
        assert_eq!(
            lane.selected(client).unwrap_err().kind,
            crate::ErrorKind::Validation
        );
        let item = |id| ViewItem::File(crate::catalog_types::FileId(id));
        lane.views.insert(
            client,
            View {
                revision: 1,
                over_files: true,
                items: (1..=5).map(item).collect(),
                layout: Default::default(),
                selection: ViewSelection {
                    count: 3,
                    ranges: vec![
                        PositionRange { start: 0, len: 1 },
                        PositionRange { start: 3, len: 2 },
                    ],
                    active: None,
                },
                stale: false,
            },
        );
        assert_eq!(lane.selected(client).unwrap(), [item(1), item(4), item(5)]);
        lane.views.get_mut(&client).unwrap().stale = true;
        assert_eq!(
            lane.selected(client).unwrap_err().kind,
            crate::ErrorKind::Conflict
        );
        lane.disconnect(client);
        assert_eq!(
            lane.selected(client).unwrap_err().kind,
            crate::ErrorKind::Validation
        );
    }

    /// A view evaluated again into the same items keeps the preview lane's job for it rather than
    /// cancelling it and reading and queuing the same previews again.
    ///
    /// The view's tasks are held at a gate until both reads of the board are made, so the job is
    /// still running when the view is evaluated again: the card's files are not on disk, so an
    /// unheld task fails within milliseconds, and a job that ends before the board is read is
    /// listed by neither read (it ran under the board's recent threshold), after which the view
    /// evaluated again rightly wants its missing previews through a new job.
    #[test]
    fn browse_a_view_read_again_keeps_its_preview_job() {
        let fx = testing::fixture("owner-same-job");
        let (owner, join) = OwnerHandle::start(&fx.catalog).unwrap();
        let client = owner.register();
        let gate = std::sync::Arc::new(luxforge_testbase::Gate::new());
        gate.shut();
        owner.hold_previews(Some(std::sync::Arc::clone(&gate)));
        let card = json!({"kind": "card", "volume_id": testing::volume("card")});
        let jobs = |owner: &OwnerHandle| -> Vec<Value> {
            let board = ok(owner, client, "activity.list", json!({}));
            board["active"]
                .as_array()
                .unwrap()
                .iter()
                .chain(board["recent"].as_array().unwrap())
                .filter(|entry| entry["kind"] == "preview.extract")
                .map(|entry| entry["job_id"].clone())
                .collect()
        };
        ok(&owner, client, "browse.view", json!({ "source": card }));
        let first = jobs(&owner);
        assert_eq!(first.len(), 1, "the view's one preview job is running");
        ok(&owner, client, "browse.view", json!({ "source": card }));
        assert_eq!(jobs(&owner), first, "the same items keep their job");
        owner.hold_previews(None);
        gate.open();
        owner.stop();
        join.join().unwrap();
    }

    /// The photographs' view over a generated catalog, in process: `browse.view` of every
    /// photograph, its first evaluation and then `LUXFORGE_VIEW_SAMPLES` (default 30) more, with the
    /// owner's memory before and after the first. Run over a copy of a catalog made by
    /// `cargo xtask generate-catalog` (the `catalog-measure` scratch's `generated/`):
    /// `LUXFORGE_VIEW_BENCH=/path/catalog.sqlite cargo test --release -p luxforge-core --lib
    /// browse_view_bench -- --ignored --nocapture`.
    #[test]
    #[ignore = "a bench over a generated catalog named by LUXFORGE_VIEW_BENCH"]
    fn browse_view_bench() {
        let Ok(catalog) = std::env::var("LUXFORGE_VIEW_BENCH") else {
            eprintln!("LUXFORGE_VIEW_BENCH names no catalog");
            return;
        };
        let samples: usize = std::env::var("LUXFORGE_VIEW_SAMPLES")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(30);
        let (owner, join) = OwnerHandle::start(std::path::Path::new(&catalog)).unwrap();
        let client = owner.register();
        let memory = |owner: &OwnerHandle| {
            ok(owner, client, "resources.read", json!({}))["memory"]["bytes"]
                .as_f64()
                .unwrap()
                / 1_048_576.0
        };
        let query = json!({"source": {"kind": "all-photographs"}});
        let before = memory(&owner);
        let started = std::time::Instant::now();
        let summary = ok(&owner, client, "browse.view", query.clone());
        let first = started.elapsed().as_secs_f64() * 1000.0;
        let after = memory(&owner);
        let mut times: Vec<f64> = (0..samples)
            .map(|_| {
                let started = std::time::Instant::now();
                ok(&owner, client, "browse.view", query.clone());
                started.elapsed().as_secs_f64() * 1000.0
            })
            .collect();
        times.sort_by(f64::total_cmp);
        let at = |q: f64| times[((times.len() as f64 * q).ceil() as usize).saturating_sub(1)];
        eprintln!(
            "photos {} items: first {first:.1} ms, p50 {:.1} ms, p95 {:.1} ms; memory {before:.1} -> {after:.1} MiB (+{:.1}), after the samples {:.1} MiB",
            summary["count"],
            at(0.5),
            at(0.95),
            after - before,
            memory(&owner),
        );
        owner.stop();
        join.join().unwrap();
    }
}
