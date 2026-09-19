//! The folder tree as a flat list of visible rows.
//!
//! The table is virtual — it only draws the rows on screen — so what it needs
//! is "row n is this node at this depth". That list is rebuilt when a folder
//! is opened or closed, not every frame.

use std::collections::HashSet;

use ferret_tree::{NodeId, Tree};

use crate::prefs::Metric;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub node: NodeId,
    pub depth: u16,
}

/// Children of `node`, largest first by the chosen measure.
pub fn sorted_children(tree: &Tree, node: NodeId, metric: Metric) -> Vec<NodeId> {
    let mut children = tree.children(node).to_vec();
    // The tree already keeps them largest-first on disk.
    if metric == Metric::Size {
        children.sort_by_key(|c| std::cmp::Reverse(tree.totals(*c).size));
    }
    children
}

/// Every row that is visible: the root, and the children of every open
/// folder, depth first.
pub fn flatten(tree: &Tree, expanded: &HashSet<NodeId>, metric: Metric) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut stack = vec![Row {
        node: tree.root(),
        depth: 0,
    }];
    while let Some(row) = stack.pop() {
        rows.push(row);
        if tree.is_dir(row.node) && expanded.contains(&row.node) {
            for child in sorted_children(tree, row.node, metric).into_iter().rev() {
                stack.push(Row {
                    node: child,
                    depth: row.depth + 1,
                });
            }
        }
    }
    rows
}

/// Open every folder above `node`, so it has a row.
pub fn open_ancestors(tree: &Tree, expanded: &mut HashSet<NodeId>, node: NodeId) {
    let mut current = node;
    while let Some(parent) = tree.parent(current) {
        expanded.insert(parent);
        current = parent;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferret_core::testing::{dir, file, index_from_specs};
    use ferret_core::ROOT_RECORD;

    fn sample() -> Tree {
        let index = index_from_specs(vec![
            dir(20, ROOT_RECORD, "a"),
            file(21, 20, "a1").sized(10 << 20),
            dir(22, ROOT_RECORD, "b"),
            file(23, 22, "b1").sized(1 << 20),
            file(24, ROOT_RECORD, "top").sized(5 << 20),
        ]);
        Tree::build(&index)
    }

    #[test]
    fn only_the_root_and_its_children_show_until_a_folder_opens() {
        let tree = sample();
        let mut expanded = HashSet::from([tree.root()]);
        let nodes: Vec<_> = flatten(&tree, &expanded, Metric::OnDisk)
            .iter()
            .map(|r| r.node)
            .collect();
        // Root, then a (10 MB), top (5 MB), b (1 MB).
        assert_eq!(nodes, vec![tree.root(), 0, 4, 2]);

        expanded.insert(0);
        let rows = flatten(&tree, &expanded, Metric::OnDisk);
        assert_eq!(rows[2], Row { node: 1, depth: 2 });
        assert_eq!(rows.len(), 5);
    }

    #[test]
    fn opening_the_ancestors_makes_a_deep_node_visible() {
        let tree = sample();
        let mut expanded = HashSet::new();
        open_ancestors(&tree, &mut expanded, 3);
        let rows = flatten(&tree, &expanded, Metric::OnDisk);
        assert!(rows.iter().any(|r| r.node == 3 && r.depth == 2));
    }
}
