//! `development-set.read`, `development-set.replace`, `development-set.add` and
//! `development-set.remove` on the owner (`crate::library::development_set`). Reading pages the
//! current set. Replacing it is session state, kept by the owner's request table for a retry and
//! announced as one event naming the revision it left; adding and removing members are library
//! changes recorded through [`change`], which names that revision too.
use super::{Call, Owner, change, retried};
use crate::{
    Error,
    api::{announce_once, methods::value},
    catalog_types::{
        AssetRowId, DevelopmentSetChanged, DevelopmentSetReplaced, MAX_SET_MEMBERS, MAX_SET_PAGE,
        SetSource, ViewItem,
        api::{DevelopmentSetMembers, DevelopmentSetRead, DevelopmentSetReplace},
    },
    library::{
        development_set::{self, CurrentSet},
        journal::{self, Request},
    },
};
use serde_json::Value;

/// `development-set.read`: members `from..from + limit` of the current set, in order, with its
/// identity, revision and size.
pub(in crate::api) fn development_set_read(
    owner: &mut Owner,
    _: &Call<'_>,
    params: DevelopmentSetRead,
) -> Result<Value, Error> {
    let limit = params.limit.unwrap_or(MAX_SET_PAGE).clamp(1, MAX_SET_PAGE);
    value(development_set::page(
        &owner.service.connection,
        params.from.unwrap_or(0),
        limit,
    )?)
}

/// `development-set.replace`: retire the current set and start one of the photographs `source`
/// names, or none. Every view is evaluated against the new set from now on.
pub(in crate::api) fn development_set_replace(
    owner: &mut Owner,
    call: &Call<'_>,
    params: DevelopmentSetReplace,
) -> Result<Value, Error> {
    let (members, active) = match &params.source {
        SetSource::Assets { asset_ids } => {
            if asset_ids.len() > MAX_SET_MEMBERS {
                return Err(too_many(asset_ids.len()));
            }
            (
                Some(development_set::rows_of(
                    &owner.service.connection,
                    asset_ids,
                )?),
                None,
            )
        }
        SetSource::View { revision, position } => {
            let items = owner.catalog.views.held(call.client, *revision)?;
            let (members, active) = around(items, *position);
            (Some(members), active)
        }
        SetSource::None => (None, None),
    };
    let set = crate::editor::write(&mut owner.service.connection, |tx| {
        development_set::replace(tx, members.as_deref())
    })?;
    super::super::views::changed(owner);
    let revision = development_set::revision(&owner.service.connection)?;
    let count = match &set {
        Some(set) => development_set::count(&owner.service.connection, set.row)?,
        None => 0,
    };
    announce_once(
        &mut owner.announced,
        &call.origin.clone().development_set(revision),
    );
    value(DevelopmentSetReplaced {
        set_id: set.map(|set| set.id),
        revision,
        count,
        active: active.filter(|_| count > 0),
        deduplicated: false,
    })
}

/// The photographs of a view's items, at most [`MAX_SET_MEMBERS`] around `position`, and where the
/// photograph at `position` is among them. A view of files holds a merge's result among its files,
/// which joins the set as a photograph does.
fn around(items: &[ViewItem], position: u32) -> (Vec<AssetRowId>, Option<u32>) {
    let photos: Vec<(usize, AssetRowId)> = items
        .iter()
        .enumerate()
        .filter_map(|(at, item)| match item {
            ViewItem::Photo(row) => Some((at, *row)),
            _ => None,
        })
        .collect();
    let opened = photos.partition_point(|(at, _)| *at < position as usize);
    let start = opened
        .saturating_sub(MAX_SET_MEMBERS / 2)
        .min(photos.len().saturating_sub(MAX_SET_MEMBERS));
    let end = (start + MAX_SET_MEMBERS).min(photos.len());
    let active = photos
        .get(opened)
        .filter(|(at, _)| *at == position as usize)
        .map(|_| (opened - start) as u32);
    (
        photos[start..end].iter().map(|(_, row)| *row).collect(),
        active,
    )
}

/// `development-set.add`: append the photographs to the current set, each once, as one library
/// change; with no set, start one of them.
pub(in crate::api) fn development_set_add(
    owner: &mut Owner,
    call: &Call<'_>,
    params: DevelopmentSetMembers,
) -> Result<Value, Error> {
    members(owner, call, params, true)
}

/// `development-set.remove`: take the photographs out of the current set as one library change.
pub(in crate::api) fn development_set_remove(
    owner: &mut Owner,
    call: &Call<'_>,
    params: DevelopmentSetMembers,
) -> Result<Value, Error> {
    members(owner, call, params, false)
}

fn members(
    owner: &mut Owner,
    call: &Call<'_>,
    params: DevelopmentSetMembers,
    adding: bool,
) -> Result<Value, Error> {
    let request = Request::new(&call.request.method, &params.mutation);
    if let Some(answer) = retried(owner, request)? {
        return answered(owner, answer);
    }
    if params.asset_ids.is_empty() {
        return Err(Error::validation("asset_ids names no photograph"));
    }
    if params.asset_ids.len() > crate::catalog_types::MAX_LIBRARY_BATCH {
        return Err(Error::resource_limit(format!(
            "{} photographs exceed the {} a library change covers",
            params.asset_ids.len(),
            crate::catalog_types::MAX_LIBRARY_BATCH
        )));
    }
    let connection = &owner.service.connection;
    let current = development_set::current(connection)?;
    development_set::check_expected(current.as_ref(), params.set_id.as_ref())?;
    if adding {
        development_set::rows_of(connection, &params.asset_ids)?;
    } else if current.is_none() {
        return answered(owner, crate::library::journal::Outcome::NoOp.answer());
    }
    let assets = params.asset_ids;
    let answer = change(owner, &call.origin, |tx| {
        let set = match current {
            Some(set) => set,
            // Nothing to add to: the photographs start a set of their own, as a Pick merged with
            // no set does.
            None => {
                development_set::replace(tx, Some(&[]))?.expect("an empty replacement starts a set")
            }
        };
        let changes = if adding {
            development_set::additions(&set, &assets)?
        } else {
            development_set::removals(&set, &assets)
        };
        journal::apply(tx, request, changes, |rows| label(adding, rows.len()))
    })?;
    answered(owner, answer)
}

/// The change's label: "Added 3 to the development set", "Removed DSC_0412 HDR from …".
fn label(adding: bool, count: usize) -> String {
    let photographs = if count == 1 {
        "1 photograph".to_owned()
    } else {
        format!("{count} photographs")
    };
    if adding {
        format!("Added {photographs} to the development set")
    } else {
        format!("Removed {photographs} from the development set")
    }
}

/// A library answer with the set as it now stands.
fn answered(owner: &Owner, answer: crate::catalog_types::LibraryAnswer) -> Result<Value, Error> {
    let connection = &owner.service.connection;
    let set: Option<CurrentSet> = development_set::current(connection)?;
    let count = match &set {
        Some(set) => development_set::count(connection, set.row)?,
        None => 0,
    };
    value(DevelopmentSetChanged {
        outcome: answer.outcome,
        change: answer.change,
        items: answer.items,
        deduplicated: answer.deduplicated,
        set_id: set.map(|set| set.id),
        revision: development_set::revision(connection)?,
        count,
    })
}

fn too_many(count: usize) -> Error {
    Error::resource_limit(format!(
        "{count} photographs exceed the {MAX_SET_MEMBERS} a development set holds"
    ))
}
