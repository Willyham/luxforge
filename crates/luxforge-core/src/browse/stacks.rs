//! HDR stacks in views (`docs/design/hdr-merge.md`, "The stack and the originals"): which active
//! stacks a source holds, how a view shows each, and the originals `stack.inspect` and expanded
//! stacks list.
//!
//! - **Reading.** Over files, the active stacks claiming a path in the source's folders
//!   (`stack_claims`, one range read per outermost folder) bring their results into the view as
//!   photographs, once each however many sources overlap; over photographs, the active stacks
//!   whose result or a developed original the source holds. Stacks are named by their merge and
//!   their originals by path and the merge's order, never by an index row, so an index rebuilt
//!   finds the same stacks.
//! - **Filtering.** Every item is filtered on its own metadata first; [`arrange`] then works on
//!   what matched. A leader that matched hides the originals the view holds while it is collapsed
//!   (`collapsed` counts them), or is followed by them, in the stack's order, once expanded;
//!   [`StackExpansion::All`] also brings in the originals the filter or the source left out,
//!   labelled `outside_filter`. Originals whose leader did not match stay where they are, each
//!   with its stack's badge.
//! - **Development set.** The current set's members are read once a view; a result in it is
//!   Picked over files and decides its bracket for Moments without a pick.
//!
//! Everything here is bookkeeping over the catalog's and the index's keys: no pixel or file is
//! read.
use super::{
    candidates::{Candidates, Extra},
    json_list,
};
use crate::{
    AssetId, EditorService, Error,
    catalog_types::{
        AssetRowId, FileAvailability, FileId, FrameFacts, MergeId, MergeKind, MergeState, RowStack,
        StackExpansion, StackInputKey, StackInspection, StackOriginal, StackRole, ViewItem,
    },
    library::development_set,
};
use rusqlite::{Connection, OptionalExtension};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

/// How each stack a client expanded is shown; a stack it never named is collapsed.
pub(crate) type StackExpansions = HashMap<MergeId, StackExpansion>;

/// The active stacks a source holds and the current development set's members.
#[derive(Default)]
pub(super) struct Stacks {
    pub list: Vec<Stack>,
    /// Each source item that is an original, with its stack's index and its ordinal.
    members: HashMap<ViewItem, (usize, u32)>,
    /// The results leading them.
    leaders: HashSet<AssetRowId>,
    /// The current development set's members.
    pub in_set: HashSet<AssetRowId>,
}

/// One active stack: its merge, its result's row, and its originals in order.
pub(super) struct Stack {
    pub merge_id: MergeId,
    pub result: AssetRowId,
    pub inputs: Vec<StackInput>,
}

/// One original: its place, EV offset, and the source's item that is it, if any.
pub(super) struct StackInput {
    pub ordinal: u32,
    pub offset_ev: f64,
    pub item: Option<ViewItem>,
}

impl Stacks {
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Whether `item` is a stack's result.
    pub fn leads(&self, item: ViewItem) -> bool {
        matches!(item, ViewItem::Photo(row) if self.leaders.contains(&row))
    }

    fn index(&mut self) {
        self.members.clear();
        self.leaders = self.list.iter().map(|stack| stack.result).collect();
        for (at, stack) in self.list.iter().enumerate() {
            for input in &stack.inputs {
                if let Some(item) = input.item {
                    self.members.insert(item, (at, input.ordinal));
                }
            }
        }
    }

    /// The stack `item` is an original of, with its ordinal.
    pub fn member(&self, item: ViewItem) -> Option<(&Stack, u32)> {
        self.members
            .get(&item)
            .map(|&(at, ordinal)| (&self.list[at], ordinal))
    }

    /// Whether `item` is an original of a result in the current development set: a decision for
    /// its bracket.
    pub fn decided_by_set(&self, item: ViewItem) -> bool {
        self.member(item)
            .is_some_and(|(stack, _)| self.in_set.contains(&stack.result))
    }
}

/// One active stack as read: its merge, result row and originals' ordinal, path, offset and
/// developed photograph's row.
pub(super) struct Loaded {
    merge_id: MergeId,
    result: AssetRowId,
    inputs: Vec<(u32, String, f64, Option<AssetRowId>)>,
}

/// The paths the active stacks claim under `folders` (each the outermost of the source's folders),
/// with the claiming merge. One range read of `stack_claims` per folder.
pub(super) fn claimed(
    connection: &Connection,
    folders: &[&str],
) -> Result<HashMap<String, MergeId>, Error> {
    let mut statement = connection
        .prepare_cached("SELECT path, merge_id FROM stack_claims WHERE path >= ?1 AND path < ?2")?;
    let mut claimed = HashMap::new();
    for folder in folders {
        let (lower, upper) = super::subtree_bounds(folder);
        let rows = statement.query_map([&lower, &upper], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (path, merge) = row?;
            claimed.insert(path, MergeId::parse(merge)?);
        }
    }
    Ok(claimed)
}

/// The statement every read of stacks shares: each active stack's originals in order, with its
/// result's row and each original's developed photograph (named by the merge, or developed from
/// its path since), not in Removed.
const STACK_ROWS: &str = "SELECT i.merge_id, r.row_id, i.ordinal, i.path, i.offset_ev,
         coalesce(a1.row_id, a2.row_id)
     FROM merge_inputs i
     JOIN merge_stacks s ON s.merge_id = i.merge_id AND s.state = 'active'
     JOIN merges m ON m.id = i.merge_id
     JOIN assets r ON r.id = m.result_asset_id AND r.removed_ms IS NULL
     LEFT JOIN assets a1 ON a1.id = i.asset_id AND a1.removed_ms IS NULL
     LEFT JOIN assets a2 ON a2.canonical_locator = i.path AND a2.removed_ms IS NULL";

fn loaded(rows: &mut rusqlite::Rows<'_>) -> Result<Vec<Loaded>, Error> {
    let mut stacks: Vec<Loaded> = Vec::new();
    while let Some(row) = rows.next()? {
        let merge: String = row.get(0)?;
        let input = (
            row.get::<_, i64>(2)? as u32,
            row.get::<_, String>(3)?,
            row.get::<_, f64>(4)?,
            row.get::<_, Option<i64>>(5)?.map(AssetRowId),
        );
        match stacks.last_mut() {
            Some(last) if last.merge_id.as_str() == merge => last.inputs.push(input),
            _ => stacks.push(Loaded {
                merge_id: MergeId::parse(merge)?,
                result: AssetRowId(row.get(1)?),
                inputs: vec![input],
            }),
        }
    }
    Ok(stacks)
}

/// The active stacks of `merges`. One read for them all.
pub(super) fn load(connection: &Connection, merges: &[MergeId]) -> Result<Vec<Loaded>, Error> {
    let mut statement = connection.prepare_cached(&format!(
        "{STACK_ROWS} WHERE i.merge_id IN (SELECT value FROM json_each(?1))
         ORDER BY i.merge_id, i.ordinal"
    ))?;
    let mut rows = statement.query([json_list(merges)?])?;
    loaded(&mut rows)
}

/// Every active stack. One read, bounded by the active stacks' originals.
pub(super) fn load_all(connection: &Connection) -> Result<Vec<Loaded>, Error> {
    let mut statement =
        connection.prepare_cached(&format!("{STACK_ROWS} ORDER BY i.merge_id, i.ordinal"))?;
    let mut rows = statement.query([])?;
    loaded(&mut rows)
}

/// The rows of the results of `loaded`, ascending, each once.
pub(super) fn results(loaded: &[Loaded]) -> Vec<i64> {
    let mut rows: Vec<i64> = loaded.iter().map(|stack| stack.result.0).collect();
    rows.sort_unstable();
    rows.dedup();
    rows
}

/// The stacks over files: each original is the source's file at its path, when the source holds
/// one.
pub(super) fn over_files(loaded: Vec<Loaded>, files: &HashMap<String, ViewItem>) -> Stacks {
    let mut stacks = Stacks {
        list: loaded
            .into_iter()
            .map(|stack| Stack {
                merge_id: stack.merge_id,
                result: stack.result,
                inputs: stack
                    .inputs
                    .into_iter()
                    .map(|(ordinal, path, offset_ev, _)| StackInput {
                        ordinal,
                        offset_ev,
                        item: files.get(&path).copied(),
                    })
                    .collect(),
            })
            .collect(),
        ..Stacks::default()
    };
    stacks.index();
    stacks
}

/// The stacks over photographs: those whose result or a developed original `holds` accepts, each
/// original the photograph developed from it.
pub(super) fn over_photos(loaded: Vec<Loaded>, holds: impl Fn(AssetRowId) -> bool) -> Stacks {
    let mut stacks = Stacks {
        list: loaded
            .into_iter()
            .filter(|stack| {
                holds(stack.result) || stack.inputs.iter().any(|input| input.3.is_some_and(&holds))
            })
            .map(|stack| Stack {
                merge_id: stack.merge_id,
                result: stack.result,
                inputs: stack
                    .inputs
                    .into_iter()
                    .map(|(ordinal, _, offset_ev, photo)| StackInput {
                        ordinal,
                        offset_ev,
                        item: photo.filter(|row| holds(*row)).map(ViewItem::Photo),
                    })
                    .collect(),
            })
            .collect(),
        ..Stacks::default()
    };
    stacks.index();
    stacks
}

/// The current development set's members. One range read of the set's members, bounded by the
/// set's size.
pub(super) fn in_set(connection: &Connection) -> Result<HashSet<AssetRowId>, Error> {
    development_set::current_rows(connection)
}

/// How a view arranged its stacks: the originals a collapsed leader hides, each stacked row's
/// badge, and its counts.
#[derive(Debug, Default)]
pub(crate) struct Arranged {
    pub hidden: Vec<ViewItem>,
    pub badges: HashMap<ViewItem, RowStack>,
    /// Leaders in the view.
    pub stacks: u32,
    /// Originals hidden beneath collapsed leaders.
    pub collapsed: u32,
}

/// Arrange the filtered, ordered `facts` by their stacks: see the module documentation. `matched`
/// is every source item's facts before filtering, by item, for the originals Show all brings back;
/// `filtered` says whether the query has any condition, without which an original no source holds
/// is shown when its leader is expanded. Answers the originals it added that no source holds
/// (provenance rows), with the facts it gave them.
pub(super) fn arrange(
    facts: &mut Vec<FrameFacts>,
    candidates: &Candidates,
    every: &HashMap<ViewItem, FrameFacts>,
    expansions: &StackExpansions,
    filtered: bool,
) -> (Arranged, Vec<(ViewItem, Extra)>) {
    let stacks = &candidates.stacks;
    let mut arranged = Arranged::default();
    let mut provenance = Vec::new();
    if stacks.is_empty() {
        return (arranged, provenance);
    }
    let visible: HashSet<ViewItem> = facts.iter().map(|fact| fact.item).collect();
    // What follows each expanded leader, and every item moved or hidden from its own place.
    let mut following: HashMap<ViewItem, Vec<FrameFacts>> = HashMap::new();
    let mut moved: HashSet<ViewItem> = HashSet::new();
    for stack in &stacks.list {
        let leader = ViewItem::Photo(stack.result);
        let exposures = stack.inputs.len() as u32;
        let original = |input: &StackInput, role, outside_filter| RowStack {
            merge_id: stack.merge_id.clone(),
            role,
            exposures,
            collapsed: 0,
            expansion: None,
            ordinal: Some(input.ordinal),
            offset_ev: Some(input.offset_ev),
            outside_filter,
        };
        if !visible.contains(&leader) {
            for input in &stack.inputs {
                if let Some(item) = input.item.filter(|item| visible.contains(item)) {
                    arranged
                        .badges
                        .insert(item, original(input, StackRole::Separate, false));
                }
            }
            continue;
        }
        let expansion = expansions.get(&stack.merge_id).copied().unwrap_or_default();
        arranged.stacks += 1;
        let mut collapsed = 0;
        let mut children = Vec::new();
        for input in &stack.inputs {
            let in_view = input.item.filter(|item| visible.contains(item));
            match (expansion, in_view, input.item) {
                (StackExpansion::Collapsed, Some(item), _) => {
                    arranged.hidden.push(item);
                    moved.insert(item);
                    collapsed += 1;
                }
                (StackExpansion::Collapsed, None, _) => {}
                (_, Some(item), _) => {
                    moved.insert(item);
                    if let Some(fact) = every.get(&item) {
                        children.push(fact.clone());
                    }
                    arranged
                        .badges
                        .insert(item, original(input, StackRole::Original, false));
                }
                // Left out by the filter: only Show all brings it in, labelled.
                (StackExpansion::All, None, Some(item)) => {
                    if let Some(fact) = every.get(&item) {
                        children.push(fact.clone());
                        arranged
                            .badges
                            .insert(item, original(input, StackRole::Original, true));
                    }
                }
                (StackExpansion::Expanded, None, Some(_)) => {}
                // No source item is this original: a provenance row, under its leader's facts.
                (expansion, None, None) => {
                    if expansion == StackExpansion::Expanded && filtered {
                        continue;
                    }
                    let item = ViewItem::Input(StackInputKey::new(stack.result, input.ordinal));
                    if let Some(fact) = every.get(&leader) {
                        children.push(FrameFacts {
                            merged: false,
                            item,
                            ..fact.clone()
                        });
                        let leader_extra = candidates.extra(leader);
                        provenance.push((
                            item,
                            Extra {
                                picked: false,
                                in_catalog: false,
                                edited: false,
                                decided: false,
                                ..*leader_extra
                            },
                        ));
                        arranged
                            .badges
                            .insert(item, original(input, StackRole::Original, filtered));
                    }
                }
            }
        }
        arranged.collapsed += collapsed;
        arranged.badges.insert(
            leader,
            RowStack {
                merge_id: stack.merge_id.clone(),
                role: StackRole::Leader,
                exposures,
                collapsed,
                expansion: Some(expansion),
                ordinal: None,
                offset_ev: None,
                outside_filter: false,
            },
        );
        if !children.is_empty() {
            following.insert(leader, children);
        }
    }
    if moved.is_empty() && following.is_empty() {
        return (arranged, provenance);
    }
    let mut ordered =
        Vec::with_capacity(facts.len() + following.values().map(Vec::len).sum::<usize>());
    for fact in facts.drain(..) {
        if moved.contains(&fact.item) {
            continue;
        }
        let children = following.remove(&fact.item);
        ordered.push(fact);
        ordered.extend(children.into_iter().flatten());
    }
    *facts = ordered;
    (arranged, provenance)
}

/// `stack.inspect`: a stack's identity, state and result, how `expansion` shows it, and its
/// originals `from..from + limit`, each with whether it is where the merge read it — a developed
/// photograph's availability, or the index's listing and its root's — its index row, pick and
/// developed photograph. Refused with `validation` for an unknown merge. Bookkeeping reads only:
/// one per original, bounded by the stack's size.
pub(crate) fn inspect(
    service: &EditorService,
    merge: &MergeId,
    from: u32,
    limit: usize,
    expansion: StackExpansion,
) -> Result<StackInspection, Error> {
    let connection = &service.connection;
    let row: Option<(String, String, String, String, Option<i64>, i64)> = connection
        .prepare_cached(
            "SELECT m.kind, s.state, a.id, a.file_name, a.removed_ms, a.row_id FROM merges m
             JOIN merge_stacks s ON s.merge_id = m.id
             JOIN assets a ON a.id = m.result_asset_id WHERE m.id = ?1",
        )?
        .query_row([merge.as_str()], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })
        .optional()?;
    let (kind, state, result, file_name, removed, result_row) =
        row.ok_or_else(|| Error::validation(format!("unknown merge {merge}")))?;
    let kind = match kind.as_str() {
        "hdr" => MergeKind::Hdr,
        other => {
            return Err(Error::incompatible(format!(
                "merge {merge}: unknown kind {other}; it is kept unchanged"
            )));
        }
    };
    let members = crate::library::merge::members(connection, merge)?;
    let exposures = members.len() as u32;
    let reference = reference_ordinal(&members);
    let page: Vec<_> = members
        .into_iter()
        .skip(from as usize)
        .take(limit)
        .collect();
    let mut originals = Vec::with_capacity(page.len());
    for member in page {
        let found = original(service, &member.path, member.asset_id.as_ref())?;
        originals.push(StackOriginal {
            ordinal: member.ordinal,
            file_name: file_name_of(&member.path),
            path: member.path,
            offset_ev: member.offset_ev,
            reference: Some(member.ordinal) == reference,
            availability: found.availability,
            reason: found.reason,
            file_id: found.file_id,
            picked: found.picked,
            asset_id: found.asset_id,
        });
    }
    Ok(StackInspection {
        merge_id: merge.clone(),
        kind,
        state: MergeState::parse(&state)?,
        result_asset_id: AssetId::parse(result)?,
        file_name,
        removed: removed.is_some(),
        in_development_set: in_set(connection)?.contains(&AssetRowId(result_row)),
        expansion,
        exposures,
        from,
        originals,
    })
}

/// The reference original: the one at no EV offset, every other's offset being from it; the
/// middle one if none is.
fn reference_ordinal(members: &[crate::catalog_types::StackMember]) -> Option<u32> {
    members
        .iter()
        .find(|member| member.offset_ev == 0.0)
        .or_else(|| members.get(members.len() / 2))
        .map(|member| member.ordinal)
}

/// What the catalog and the index say of one original now.
pub(super) struct Found {
    pub availability: FileAvailability,
    pub reason: Option<String>,
    pub file_id: Option<FileId>,
    pub picked: bool,
    pub asset_id: Option<AssetId>,
}

/// One original's availability, index row, pick and developed photograph: the photograph's
/// recorded availability when it is developed, otherwise the index's listing under its root's
/// state. Indexed point reads of the catalog and the index.
pub(super) fn original(
    service: &EditorService,
    path: &Path,
    asset: Option<&AssetId>,
) -> Result<Found, Error> {
    let text = path.to_string_lossy();
    let connection = &service.connection;
    let developed: Option<(String, String)> = connection
        .prepare_cached(
            "SELECT id, availability FROM assets WHERE removed_ms IS NULL
                 AND (id = ?1 OR canonical_locator = ?2) LIMIT 1",
        )?
        .query_row(
            [asset.map(AssetId::as_str).unwrap_or(""), text.as_ref()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let picked: bool = connection
        .prepare_cached("SELECT EXISTS(SELECT 1 FROM picks WHERE path = ?1)")?
        .query_row([text.as_ref()], |row| row.get(0))?;
    let index = service.index()?;
    let listed: Option<(i64, String)> = index
        .connection()
        .prepare_cached("SELECT id, folder FROM files WHERE path = ?1")?
        .query_row([text.as_ref()], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()?;
    let offline_root: Option<String> = {
        let mut statement = index
            .connection()
            .prepare_cached("SELECT path FROM roots WHERE offline = 1")?;
        let roots = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut found = None;
        for root in roots {
            let root = root?;
            if super::within(&text, &root) {
                found = Some(root);
                break;
            }
        }
        found
    };
    drop(index);
    let (availability, reason) = match (&developed, &listed, &offline_root) {
        (Some((_, availability)), _, _) => {
            let availability =
                FileAvailability::parse(availability).unwrap_or(FileAvailability::Missing);
            let reason = match availability {
                FileAvailability::Available => None,
                FileAvailability::Offline => Some("its volume is offline".to_owned()),
                FileAvailability::Missing => Some("its file is missing".to_owned()),
                FileAvailability::Changed => {
                    Some("its file changed since it was developed".to_owned())
                }
            };
            (availability, reason)
        }
        (None, _, Some(_)) => (
            FileAvailability::Offline,
            Some("its volume is offline".to_owned()),
        ),
        (None, Some(_), None) => (FileAvailability::Available, None),
        (None, None, None) => (
            FileAvailability::Missing,
            Some("no indexed folder or card lists it".to_owned()),
        ),
    };
    Ok(Found {
        availability,
        reason,
        file_id: listed.map(|(id, _)| FileId(id)),
        picked,
        asset_id: developed.map(|(id, _)| AssetId::parse(id)).transpose()?,
    })
}

/// The original `key` names: its merge, path and the photograph the merge recorded for it. One
/// indexed read.
pub(super) fn input_of(
    connection: &Connection,
    key: StackInputKey,
) -> Result<Option<(MergeId, PathBuf, Option<AssetId>)>, Error> {
    let row: Option<(String, String, Option<String>)> = connection
        .prepare_cached(
            "SELECT i.merge_id, i.path, i.asset_id FROM assets a
             JOIN merges m ON m.result_asset_id = a.id
             JOIN merge_inputs i ON i.merge_id = m.id AND i.ordinal = ?2
             WHERE a.row_id = ?1",
        )?
        .query_row([key.result().0, i64::from(key.ordinal())], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .optional()?;
    row.map(|(merge, path, asset)| {
        Ok((
            MergeId::parse(merge)?,
            PathBuf::from(path),
            asset.map(AssetId::parse).transpose()?,
        ))
    })
    .transpose()
}

fn file_name_of(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}
