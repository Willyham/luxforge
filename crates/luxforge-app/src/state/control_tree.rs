//! The one walk over a control tree: a module's declared [`Control`]s, or the panel's
//! [`ControlModel`]s derived from them. Every question the desktop asks of a tree — which field a
//! control edits, which label it carries, whether a section holds a curve, which group sits at a
//! path — is a filter over this walk, so the order and the descent into groups are defined once.
use crate::state::tools::ControlModel;
use luxforge_core::Control;

/// A control tree: a node is a group exactly when it has children.
pub(crate) trait ControlTree: Sized {
    fn children(&self) -> Option<&[Self]>;
}

impl ControlTree for Control {
    fn children(&self) -> Option<&[Self]> {
        match self {
            Control::Group { controls, .. } => Some(controls),
            _ => None,
        }
    }
}

impl ControlTree for ControlModel {
    fn children(&self) -> Option<&[Self]> {
        match self {
            ControlModel::Group(group) => Some(&group.controls),
            _ => None,
        }
    }
}

/// Every control in declaration order, depth first, each group before its children.
///
/// It is an iterator, so a search is `walk(controls).find_map(…)`. A caller that needs the
/// position of the control it was just given reads [`Walk::path`], and one that must not look
/// inside a group, such as a collapsed one, calls [`Walk::skip_children`] before asking for the
/// next control. It allocates one frame per open group and nothing per control.
pub(crate) fn walk<T: ControlTree>(controls: &[T]) -> Walk<'_, T> {
    Walk {
        stack: vec![(controls, 0)],
        last: None,
        skip: false,
    }
}

/// The node at an index path, such as a group reset or a disclosure names: each index but the
/// last is a group's.
pub(crate) fn at_path<'a, T: ControlTree>(controls: &'a [T], path: &[usize]) -> Option<&'a T> {
    let (last, groups) = path.split_last()?;
    let mut level = controls;
    for index in groups {
        level = level.get(*index)?.children()?;
    }
    level.get(*last)
}

pub(crate) struct Walk<'a, T> {
    /// The open levels: each slice and the index of the next node to visit in it.
    stack: Vec<(&'a [T], usize)>,
    /// The node returned last, whose children are entered on the next call.
    last: Option<&'a T>,
    skip: bool,
}

impl<T: ControlTree> Walk<'_, T> {
    /// The index path of the node returned last: its index in each enclosing level.
    pub(crate) fn path(&self) -> Vec<usize> {
        self.stack.iter().map(|(_, next)| next - 1).collect()
    }

    /// Do not visit the children of the node returned last.
    pub(crate) fn skip_children(&mut self) {
        self.skip = true;
    }
}

impl<'a, T: ControlTree> Iterator for Walk<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<&'a T> {
        if let Some(children) = self.last.take().and_then(ControlTree::children)
            && !self.skip
        {
            self.stack.push((children, 0));
        }
        self.skip = false;
        loop {
            let (level, next) = self.stack.last_mut()?;
            if let Some(node) = level.get(*next) {
                *next += 1;
                self.last = Some(node);
                return Some(node);
            }
            self.stack.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(label: &str, controls: Vec<Control>) -> Control {
        Control::group(label, controls)
    }

    fn picker(label: &str) -> Control {
        Control::picker(label)
    }

    fn label(control: &Control) -> &str {
        match control {
            Control::Group { label, .. } | Control::Picker { label } => label,
            _ => unreachable!("only groups and pickers are built here"),
        }
    }

    /// Depth first in declaration order, each group before its children, with the path of each
    /// node, and a skipped group's children left out without losing its siblings.
    #[test]
    fn the_walk_visits_every_node_in_order_with_its_path() {
        let tree = vec![
            group("a", vec![picker("a0"), group("a1", vec![picker("a10")])]),
            picker("b"),
            group("c", vec![picker("c0")]),
        ];
        let mut visited = Vec::new();
        let mut walker = walk(&tree);
        while let Some(control) = walker.next() {
            visited.push((label(control).to_owned(), walker.path()));
        }
        assert_eq!(
            visited,
            [
                ("a", vec![0]),
                ("a0", vec![0, 0]),
                ("a1", vec![0, 1]),
                ("a10", vec![0, 1, 0]),
                ("b", vec![1]),
                ("c", vec![2]),
                ("c0", vec![2, 0]),
            ]
            .map(|(label, path)| (label.to_owned(), path))
        );

        let mut kept = Vec::new();
        let mut walker = walk(&tree);
        while let Some(control) = walker.next() {
            if label(control) == "a" {
                walker.skip_children();
            }
            kept.push(label(control).to_owned());
        }
        assert_eq!(kept, ["a", "b", "c", "c0"]);

        assert_eq!(at_path(&tree, &[0, 1, 0]).map(label), Some("a10"));
        assert_eq!(at_path(&tree, &[2]).map(label), Some("c"));
        assert!(
            at_path(&tree, &[1, 0]).is_none(),
            "a picker has no children"
        );
        assert!(at_path(&tree, &[]).is_none());
        assert!(walk::<Control>(&[]).next().is_none());
    }
}
