//! What catalog folders and collections share as named trees (`docs/design/catalog.md`, "The
//! catalog"): a name's rules, the clash between siblings, whether one node is inside another, the
//! parents-first order their lists answer in, and a planned library change with its label.
//!
//! A name is trimmed and holds 1 to [`MAX_LIBRARY_NAME`] characters and no control character; it
//! is unique among its siblings ignoring case, the top level being one parent. Case is folded as
//! Unicode lowercase, which refuses every clash the catalog's own `NOCASE` index would and a few it
//! would not ("Ärger" and "ärger"), so a refusal is always this module's clear one rather than the
//! schema's.
use super::journal::{self, Desired, Outcome, Request};
use crate::{
    Error,
    catalog_types::{LibraryChangeRow, LibraryItem, MAX_LIBRARY_NAME},
};
use rusqlite::{Connection, OptionalExtension, Transaction};
use std::collections::{HashMap, HashSet};

/// A name as the catalog keeps it: trimmed, 1 to [`MAX_LIBRARY_NAME`] characters, no control
/// character.
pub(crate) fn checked_name(raw: &str) -> Result<String, Error> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(Error::validation("a name must not be empty"));
    }
    if name.chars().count() > MAX_LIBRARY_NAME {
        return Err(Error::validation(format!(
            "a name is at most {MAX_LIBRARY_NAME} characters"
        )));
    }
    if name.chars().any(char::is_control) {
        return Err(Error::validation(
            "a name must not contain control characters",
        ));
    }
    Ok(name.to_owned())
}

/// Whether two names are the same ignoring case.
pub(crate) fn same_name(a: &str, b: &str) -> bool {
    a == b || a.to_lowercase() == b.to_lowercase()
}

/// The sibling (its id and name) whose name is `name` ignoring case, other than `except`.
pub(crate) fn clash<'a>(
    siblings: &'a [(String, String)],
    name: &str,
    except: Option<&str>,
) -> Option<&'a (String, String)> {
    siblings
        .iter()
        .find(|(id, sibling)| Some(id.as_str()) != except && same_name(sibling, name))
}

/// Whether `node` is `ancestor` or inside it, walking up from `node` by `parent_of` (a node's
/// parent, or none at the top level). `O(depth)` lookups; a cycle in stored data ends the walk
/// rather than looping.
pub(crate) fn within(
    node: &str,
    ancestor: &str,
    mut parent_of: impl FnMut(&str) -> Result<Option<String>, Error>,
) -> Result<bool, Error> {
    let mut seen = HashSet::new();
    let mut current = node.to_owned();
    loop {
        if current == ancestor {
            return Ok(true);
        }
        if !seen.insert(current.clone()) {
            return Ok(false);
        }
        match parent_of(&current)? {
            Some(parent) => current = parent,
            None => return Ok(false),
        }
    }
}

/// The parent of the row `id` in `table` (`catalog_folders` or `collections`).
pub(crate) fn parent_in(
    connection: &Connection,
    table: &'static str,
    id: &str,
) -> Result<Option<String>, Error> {
    Ok(connection
        .prepare_cached(&format!("SELECT parent_id FROM {table} WHERE id = ?1"))?
        .query_row([id], |row| row.get::<_, Option<String>>(0))
        .optional()?
        .flatten())
}

/// Admit a stored tree value against the tree as it stands now. Undo and redo restore values
/// through the item writer, without running the original create or move planner, so they must
/// check the parent and Unicode sibling-name invariants here too. The schema covers missing
/// parents and ordinary ASCII name clashes; these are the invariants it cannot enforce.
pub(crate) fn check_placement(
    connection: &Connection,
    table: &'static str,
    id: &str,
    name: &str,
    parent: Option<&str>,
) -> Result<(), Error> {
    if let Some(parent) = parent
        && within(parent, id, |node| parent_in(connection, table, node))?
    {
        return Err(Error::conflict(format!(
            "{name} cannot be placed inside itself or one of its descendants"
        )));
    }
    let mut statement = connection.prepare_cached(&format!(
        "SELECT name FROM {table} WHERE parent_id IS ?1 AND id <> ?2"
    ))?;
    let mut siblings = statement.query(rusqlite::params![parent, id])?;
    while let Some(sibling) = siblings.next()? {
        let existing: String = sibling.get(0)?;
        if same_name(name, &existing) {
            return Err(Error::conflict(format!(
                "{name} cannot be placed here: a sibling is already named {existing}"
            )));
        }
    }
    Ok(())
}

/// The English short names of the months, as day folders and event names write them.
pub(crate) const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Where a name sorts among its siblings by name alone: ignoring case, then as written.
pub(crate) fn by_name(name: &str) -> (String, String) {
    (name.to_lowercase(), name.to_owned())
}

/// Where a catalog folder's name sorts among its siblings (`folder.list`): years (four digits)
/// newest first, then day folders ("12 Sep", a day of 1 to 31 and an English short month, as
/// Develop names them) in calendar order, then every other name by [`by_name`].
pub(crate) fn folder_order(name: &str) -> (u8, i32, i32, (String, String)) {
    if let Some(year) = year_name(name) {
        return (0, -year, 0, (String::new(), String::new()));
    }
    if let Some((month, day)) = day_name(name) {
        return (1, month as i32, day as i32, by_name(name));
    }
    (2, 0, 0, by_name(name))
}

/// The year a name that is exactly four digits names ("2026").
fn year_name(name: &str) -> Option<i32> {
    (name.len() == 4 && name.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| name.parse().ok())
        .flatten()
}

/// The month (1 to 12) and day (1 to 31) a day folder's name names: the day without a leading zero,
/// one space and an English short month, its case ignored ("3 Sep", "12 nov").
fn day_name(name: &str) -> Option<(u32, u32)> {
    let (day, month) = name.split_once(' ')?;
    if day.is_empty() || day.len() > 2 || day.starts_with('0') {
        return None;
    }
    let day: u32 = day.parse().ok().filter(|day| (1..=31).contains(day))?;
    let month = MONTHS
        .iter()
        .position(|known| known.eq_ignore_ascii_case(month))?;
    Some((month as u32 + 1, day))
}

/// `items` parents first: each node directly followed by its subtree, siblings by `order` (then by
/// identity, so the order is total). A node whose parent is not listed is placed at the top level,
/// and nodes a stored cycle keeps from the top level come last, so a list never leaves one out.
/// `O(n log n)`.
pub(crate) fn parents_first<T, K: Ord>(
    mut items: Vec<T>,
    id: impl Fn(&T) -> &str,
    parent: impl Fn(&T) -> Option<&str>,
    order: impl Fn(&T) -> K,
) -> Vec<T> {
    items.sort_by_cached_key(|item| (order(item), id(item).to_owned()));
    let index: HashMap<&str, usize> = items
        .iter()
        .enumerate()
        .map(|(at, item)| (id(item), at))
        .collect();
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); items.len()];
    let mut roots = Vec::new();
    for (at, item) in items.iter().enumerate() {
        match parent(item).and_then(|parent| index.get(parent)) {
            Some(&parent) if parent != at => children[parent].push(at),
            _ => roots.push(at),
        }
    }
    let mut order = Vec::with_capacity(items.len());
    let mut placed = vec![false; items.len()];
    let mut walk = |start: usize, order: &mut Vec<usize>| {
        let mut stack = vec![start];
        while let Some(at) = stack.pop() {
            if std::mem::replace(&mut placed[at], true) {
                continue;
            }
            order.push(at);
            stack.extend(children[at].iter().rev());
        }
    };
    for root in roots {
        walk(root, &mut order);
    }
    for at in 0..items.len() {
        walk(at, &mut order);
    }
    let mut slots: Vec<Option<T>> = items.into_iter().map(Some).collect();
    order
        .into_iter()
        .filter_map(|at| slots[at].take())
        .collect()
}

/// How a planned change is labelled from the rows it records, with the catalog to read names from.
type Label = Box<dyn FnOnce(&Connection, &[LibraryChangeRow]) -> String>;

/// One library change, planned: what each item is to become, in an order whose reverse is also
/// valid, and how it is labelled.
pub(crate) struct Planned {
    pub changes: Vec<(LibraryItem, Desired)>,
    label: Label,
}

impl Planned {
    /// A change whose label is known before it is recorded.
    pub(crate) fn labelled(changes: Vec<(LibraryItem, Desired)>, label: String) -> Self {
        Self {
            changes,
            label: Box::new(move |_, _| label),
        }
    }

    /// A change labelled from the rows it records, which leave out the items already at their
    /// value: "Moved 5 photographs to Konstanz" names only the ones that moved.
    pub(crate) fn counted(
        changes: Vec<(LibraryItem, Desired)>,
        label: impl FnOnce(&Connection, &[LibraryChangeRow]) -> String + 'static,
    ) -> Self {
        Self {
            changes,
            label: Box::new(label),
        }
    }

    /// Record the change through the journal in the caller's transaction.
    pub(crate) fn apply(
        self,
        tx: &Transaction<'_>,
        request: Request<'_>,
    ) -> Result<Outcome, Error> {
        let Self { changes, label } = self;
        journal::apply(tx, request, changes, |rows| label(tx, rows))
    }
}

/// The photographs a change's rows name, as a label says them: the file name of one ("DSC_0412.NEF"),
/// or how many (`many(5)`).
pub(crate) fn photographs(
    connection: &Connection,
    rows: &[LibraryChangeRow],
    many: impl FnOnce(usize) -> String,
) -> String {
    let [row] = rows else {
        return many(rows.len());
    };
    let asset = match &row.item {
        LibraryItem::AssetFolder { asset_id }
        | LibraryItem::Membership { asset_id, .. }
        | LibraryItem::AssetRemoval { asset_id } => asset_id,
        other => return other.key(),
    };
    connection
        .prepare_cached("SELECT file_name FROM assets WHERE id = ?1")
        .and_then(|mut statement| {
            statement
                .query_row([asset.as_str()], |row| row.get::<_, String>(0))
                .optional()
        })
        .ok()
        .flatten()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| asset.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    struct Node(&'static str, Option<&'static str>, &'static str);

    fn ordered(nodes: Vec<Node>) -> Vec<&'static str> {
        parents_first(nodes, |node| node.0, |node| node.1, |node| by_name(node.2))
            .into_iter()
            .map(|node| node.0)
            .collect()
    }

    #[test]
    fn catalog_folder_names_are_trimmed_bounded_and_printable() {
        assert_eq!(
            checked_name("  Konstanz · Sep 2026 ").unwrap(),
            "Konstanz · Sep 2026"
        );
        assert_eq!(
            checked_name(" \t ").unwrap_err().kind,
            crate::ErrorKind::Validation
        );
        assert_eq!(
            checked_name("a\u{7}b").unwrap_err().kind,
            crate::ErrorKind::Validation
        );
        let longest = "é".repeat(MAX_LIBRARY_NAME);
        assert_eq!(checked_name(&format!(" {longest} ")).unwrap(), longest);
        assert!(checked_name(&format!("{longest}e")).is_err());
        assert!(same_name("Ärger", "ärger"));
        assert!(same_name("KONSTANZ", "konstanz"));
        assert!(!same_name("Konstanz", "Konstanz 2"));
        let siblings = vec![("a".to_owned(), "Portfolio".to_owned())];
        assert!(clash(&siblings, "portfolio", None).is_some());
        assert!(clash(&siblings, "portfolio", Some("a")).is_none(), "itself");
    }

    #[test]
    fn catalog_folder_trees_list_parents_first_with_siblings_by_name() {
        let order = ordered(vec![
            Node("c", Some("a"), "zeta"),
            Node("b", None, "beta"),
            Node("a", None, "Alpha"),
            Node("d", Some("a"), "Eta"),
            Node("e", Some("d"), "x"),
            // An orphan is at the top level; a stored cycle is still listed, last.
            Node("f", Some("gone"), "aardvark"),
            Node("g", Some("h"), "g"),
            Node("h", Some("g"), "h"),
        ]);
        assert_eq!(order, ["f", "a", "d", "e", "c", "b", "g", "h"]);
        let walk = |node: &str| -> Result<Option<String>, Error> {
            Ok(match node {
                "e" => Some("d".into()),
                "d" => Some("a".into()),
                "g" => Some("h".into()),
                "h" => Some("g".into()),
                _ => None,
            })
        };
        assert!(within("e", "a", walk).unwrap());
        assert!(within("a", "a", walk).unwrap());
        assert!(!within("a", "e", walk).unwrap());
        assert!(!within("g", "a", walk).unwrap(), "a cycle ends the walk");
    }

    #[test]
    fn catalog_folder_siblings_order_years_newest_first_then_days_then_names() {
        let mut names = vec![
            "Konstanz", "12 Sep", "2025", "Undated", "3 Sep", "2026", "1 Jan", "alps", "03 Sep",
            "32 Sep", "12 sep", "2024", "Zürich", "12 Nov", "20266",
        ];
        names.sort_by_key(|name| folder_order(name));
        assert_eq!(
            names,
            [
                "2026", "2025", "2024", "1 Jan", "3 Sep", "12 Sep", "12 sep", "12 Nov", "03 Sep",
                "20266", "32 Sep", "alps", "Konstanz", "Undated", "Zürich",
            ]
        );
    }
}
