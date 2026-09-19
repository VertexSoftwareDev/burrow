//! The shape of a disk: how much every folder holds, all the way down.
//!
//! A Ferret [`Index`] is a flat list of entries, each pointing at its parent.
//! That is the right layout for searching and the wrong one for asking "what
//! is in this folder, and how big is it?". [`Tree`] adds the other direction —
//! children for every folder — and the recursive totals, in two linear passes.
//!
//! # Node ids
//!
//! A node id is the entry's position in the index, so anything the index can
//! say about an entry (its name, its path) is one lookup away. The volume root
//! is not an entry in a default scan, so it gets the id one past the last
//! entry: [`Tree::root`].

#![cfg(windows)]

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use ferret_core::{Entry, Index, ROOT_RECORD};

pub mod cleanup;
pub mod dupes;
pub mod kinds;
pub mod snapshot;

pub use kinds::Kind;

/// Position of an entry in its index, or [`Tree::root`].
pub type NodeId = u32;

const NONE: u32 = u32::MAX;

/// What a file or folder adds up to.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Totals {
    /// Logical bytes: the sum of what Explorer calls "Size".
    pub size: u64,
    /// Clusters occupied: the sum of "Size on disk". This is the number that
    /// explains a full drive, so it is what everything is ranked by.
    pub allocated: u64,
    /// Files at any depth below; 1 for a file itself.
    pub files: u32,
    /// Folders at any depth below, not counting the folder itself.
    pub dirs: u32,
    /// Latest modification anywhere inside, as a FILETIME.
    pub newest: u64,
}

impl Totals {
    fn add(&mut self, other: &Totals) {
        self.size += other.size;
        self.allocated += other.allocated;
        self.files += other.files;
        self.dirs += other.dirs;
        self.newest = self.newest.max(other.newest);
    }
}

/// Recursive totals and child lists for every folder of one index.
pub struct Tree {
    root: NodeId,
    /// Parent of every node; [`NONE`] for the root and for anything that is
    /// not part of the tree (deleted, orphaned, or an alias of the root).
    parent: Vec<u32>,
    /// `children[child_start[n]..child_start[n + 1]]` are node `n`'s children,
    /// largest first.
    child_start: Vec<u32>,
    children: Vec<u32>,
    totals: Vec<Totals>,
    is_dir: Vec<bool>,
}

impl Tree {
    /// Build the tree for `index`. Linear in the number of entries.
    pub fn build(index: &Index) -> Tree {
        let entries = index.entries();
        let count = entries.len();
        let root = count as NodeId;
        let nodes = count + 1;

        // Who is whose parent.
        let mut parent = vec![NONE; nodes];
        for (position, entry) in entries.iter().enumerate() {
            if entry.is_deleted() || entry.record == ROOT_RECORD {
                // A scan that includes metafiles also includes the root
                // directory itself; the synthetic root stands in for it.
                continue;
            }
            parent[position] = if entry.parent == ROOT_RECORD {
                root
            } else {
                match index.by_record(entry.parent) {
                    Some((p, e)) if !e.is_deleted() && e.is_dir() => p as u32,
                    _ => NONE,
                }
            };
        }

        // Children, as one flat array sliced per parent.
        let mut child_start = vec![0u32; nodes + 1];
        for p in parent.iter().filter(|p| **p != NONE) {
            child_start[*p as usize + 1] += 1;
        }
        for i in 1..child_start.len() {
            child_start[i] += child_start[i - 1];
        }
        let mut fill = child_start.clone();
        let mut children = vec![0u32; child_start[nodes] as usize];
        for (node, p) in parent.iter().enumerate() {
            if *p != NONE {
                let slot = &mut fill[*p as usize];
                children[*slot as usize] = node as u32;
                *slot += 1;
            }
        }

        // Breadth-first from the root. Whatever this does not reach is not in
        // the tree — which also means a parent cycle, however it arose, can
        // never be summed forever.
        let mut order: Vec<u32> = Vec::with_capacity(nodes);
        order.push(root);
        let mut head = 0;
        while head < order.len() {
            let node = order[head] as usize;
            head += 1;
            let range = child_start[node] as usize..child_start[node + 1] as usize;
            order.extend_from_slice(&children[range]);
        }
        // Anything not reached is cut loose, so `contains` and the parent
        // chain agree with what was summed.
        let mut reached = vec![false; nodes];
        for &node in &order {
            reached[node as usize] = true;
        }
        for (node, p) in parent.iter_mut().enumerate() {
            if !reached[node] {
                *p = NONE;
            }
        }

        let mut is_dir = vec![false; nodes];
        is_dir[count] = true;
        let mut totals = vec![Totals::default(); nodes];
        for &node in &order {
            if node == root {
                continue;
            }
            let entry = &entries[node as usize];
            is_dir[node as usize] = entry.is_dir();
            totals[node as usize] = own_totals(entry);
        }

        // Reverse breadth-first order visits every child before its parent.
        for &node in order.iter().rev() {
            let p = parent[node as usize];
            if p != NONE {
                let own = totals[node as usize];
                let target = &mut totals[p as usize];
                target.add(&own);
                if is_dir[node as usize] {
                    target.dirs += 1;
                }
            }
        }

        // Largest first, so a folder view and a treemap both read straight off.
        for node in 0..nodes {
            let range = child_start[node] as usize..child_start[node + 1] as usize;
            children[range].sort_unstable_by_key(|c| Reverse(totals[*c as usize].allocated));
        }

        Tree {
            root,
            parent,
            child_start,
            children,
            totals,
            is_dir,
        }
    }

    pub fn root(&self) -> NodeId {
        self.root
    }

    pub fn totals(&self, node: NodeId) -> Totals {
        self.totals.get(node as usize).copied().unwrap_or_default()
    }

    pub fn is_dir(&self, node: NodeId) -> bool {
        self.is_dir.get(node as usize).copied().unwrap_or(false)
    }

    /// Children of `node`, largest on disk first. Empty for a file.
    pub fn children(&self, node: NodeId) -> &[NodeId] {
        let node = node as usize;
        if node + 1 >= self.child_start.len() {
            return &[];
        }
        &self.children[self.child_start[node] as usize..self.child_start[node + 1] as usize]
    }

    pub fn parent(&self, node: NodeId) -> Option<NodeId> {
        match self.parent.get(node as usize) {
            Some(&p) if p != NONE => Some(p),
            _ => None,
        }
    }

    /// Whether `node` is reachable from the root.
    pub fn contains(&self, node: NodeId) -> bool {
        node == self.root || self.parent(node).is_some()
    }

    /// From the root down to `node`, both included.
    pub fn ancestry(&self, node: NodeId) -> Vec<NodeId> {
        let mut chain = vec![node];
        let mut current = node;
        while let Some(p) = self.parent(current) {
            chain.push(p);
            current = p;
            if chain.len() > 4096 {
                break;
            }
        }
        chain.reverse();
        chain
    }

    /// Display name: the entry's own name, or `C:\` for the root.
    pub fn name<'a>(&self, index: &'a Index, node: NodeId) -> std::borrow::Cow<'a, str> {
        if node == self.root {
            std::borrow::Cow::Owned(format!("{}:\\", index.letter))
        } else {
            std::borrow::Cow::Borrowed(index.name(node as usize))
        }
    }

    /// Full path, e.g. `C:\Users\pc`.
    pub fn path(&self, index: &Index, node: NodeId) -> String {
        if node == self.root {
            format!("{}:\\", index.letter)
        } else {
            index.full_path(node as usize).unwrap_or_default()
        }
    }

    /// The `n` largest files on disk, largest first.
    pub fn largest_files(&self, n: usize) -> Vec<NodeId> {
        self.largest(n, |tree, node| !tree.is_dir(node))
    }

    /// The `n` folders holding the most bytes *directly* — not counting
    /// subfolders. Recursive totals would just list every ancestor of the one
    /// big folder; this finds the folders where the files actually are.
    pub fn heaviest_folders(&self, n: usize) -> Vec<(NodeId, u64)> {
        let mut heap: BinaryHeap<Reverse<(u64, NodeId)>> = BinaryHeap::with_capacity(n + 1);
        for node in 0..self.totals.len() as NodeId {
            if !self.is_dir(node) || !self.contains(node) {
                continue;
            }
            let own: u64 = self
                .children(node)
                .iter()
                .filter(|c| !self.is_dir(**c))
                .map(|c| self.totals[*c as usize].allocated)
                .sum();
            push_bounded(&mut heap, n, (own, node));
        }
        let mut out: Vec<(NodeId, u64)> =
            heap.into_iter().map(|Reverse((b, id))| (id, b)).collect();
        out.sort_unstable_by_key(|(_, bytes)| Reverse(*bytes));
        out
    }

    fn largest(&self, n: usize, keep: impl Fn(&Tree, NodeId) -> bool) -> Vec<NodeId> {
        let mut heap: BinaryHeap<Reverse<(u64, NodeId)>> = BinaryHeap::with_capacity(n + 1);
        for node in 0..self.root {
            if self.contains(node) && keep(self, node) {
                push_bounded(&mut heap, n, (self.totals[node as usize].allocated, node));
            }
        }
        let mut out: Vec<(u64, NodeId)> = heap.into_iter().map(|Reverse(x)| x).collect();
        out.sort_unstable_by_key(|(bytes, _)| Reverse(*bytes));
        out.into_iter().map(|(_, id)| id).collect()
    }

    /// Where the bytes under `node` go, by kind of file, largest first.
    pub fn kinds_under(&self, index: &Index, node: NodeId) -> Vec<(Kind, Totals)> {
        let mut by_kind: HashMap<Kind, Totals> = HashMap::new();
        let mut stack = vec![node];
        while let Some(current) = stack.pop() {
            if self.is_dir(current) {
                stack.extend_from_slice(self.children(current));
            } else {
                let kind = Kind::of(index.name(current as usize));
                by_kind.entry(kind).or_default().add(&self.totals(current));
            }
        }
        let mut out: Vec<(Kind, Totals)> = by_kind.into_iter().collect();
        out.sort_unstable_by_key(|(_, t)| Reverse(t.allocated));
        out
    }

    /// Where the bytes under `node` go, by file extension, largest first.
    /// Extensions are lowercased; files without one are grouped under `""`.
    pub fn extensions_under(&self, index: &Index, node: NodeId) -> Vec<(String, Totals)> {
        let mut by_ext: HashMap<String, Totals> = HashMap::new();
        let mut key = String::new();
        let mut stack = vec![node];
        while let Some(current) = stack.pop() {
            if self.is_dir(current) {
                stack.extend_from_slice(self.children(current));
                continue;
            }
            key.clear();
            key.push_str(kinds::extension(index.name(current as usize)));
            key.make_ascii_lowercase();
            let totals = self.totals(current);
            match by_ext.get_mut(key.as_str()) {
                Some(slot) => slot.add(&totals),
                None => {
                    by_ext.insert(key.clone(), totals);
                }
            }
        }
        let mut out: Vec<(String, Totals)> = by_ext.into_iter().collect();
        out.sort_unstable_by_key(|(_, t)| Reverse(t.allocated));
        out
    }

    /// Bytes the tree holds, for memory reporting.
    pub fn memory_bytes(&self) -> usize {
        self.parent.len() * 4
            + self.child_start.len() * 4
            + self.children.len() * 4
            + self.totals.len() * std::mem::size_of::<Totals>()
            + self.is_dir.len()
    }
}

fn own_totals(entry: &Entry) -> Totals {
    if entry.is_dir() {
        // A folder's own clusters are its index; its size is its contents.
        Totals {
            allocated: entry.allocated,
            newest: entry.modified,
            ..Totals::default()
        }
    } else {
        Totals {
            size: entry.size,
            allocated: entry.allocated,
            files: 1,
            dirs: 0,
            newest: entry.modified,
        }
    }
}

fn push_bounded(heap: &mut BinaryHeap<Reverse<(u64, NodeId)>>, n: usize, item: (u64, NodeId)) {
    if n == 0 {
        return;
    }
    if heap.len() < n {
        heap.push(Reverse(item));
    } else if let Some(Reverse(smallest)) = heap.peek() {
        if item.0 > smallest.0 {
            heap.pop();
            heap.push(Reverse(item));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferret_core::mft::{IS_DELETED, IS_DIR};
    use ferret_core::testing::{dir, file, index_from_specs};

    /// ```text
    /// C:\
    /// ├── Users (20)
    /// │   └── pc (21)
    /// │       ├── movie.mkv  (30)  8 GB
    /// │       └── notes.txt  (31)  1 KB
    /// ├── Windows (22)
    /// │   └── big.log (32)  2 GB
    /// └── pagefile.sys (33)  4 GB
    /// ```
    fn sample() -> Index {
        index_from_specs(vec![
            dir(20, ROOT_RECORD, "Users"),
            dir(21, 20, "pc"),
            dir(22, ROOT_RECORD, "Windows"),
            file(30, 21, "movie.mkv").sized(8 << 30),
            file(31, 21, "notes.txt").sized(1000),
            file(32, 22, "big.log").sized(2 << 30),
            file(33, ROOT_RECORD, "pagefile.sys").sized(4 << 30),
        ])
    }

    #[test]
    fn totals_add_up_through_every_level() {
        let index = sample();
        let tree = Tree::build(&index);
        let root = tree.totals(tree.root());
        assert_eq!(root.files, 4);
        assert_eq!(root.dirs, 3);
        assert_eq!(root.allocated, (8 << 30) + 4096 + (2 << 30) + (4 << 30));

        let users = tree.totals(0);
        assert_eq!(users.files, 2);
        assert_eq!(users.dirs, 1);
        assert_eq!(users.size, (8 << 30) + 1000);
    }

    #[test]
    fn children_come_largest_first() {
        let index = sample();
        let tree = Tree::build(&index);
        let names: Vec<_> = tree
            .children(tree.root())
            .iter()
            .map(|c| tree.name(&index, *c).into_owned())
            .collect();
        assert_eq!(names, ["Users", "pagefile.sys", "Windows"]);
    }

    #[test]
    fn deleted_entries_do_not_count() {
        let index = index_from_specs(vec![
            dir(20, ROOT_RECORD, "a"),
            file(21, 20, "gone.bin")
                .sized(1 << 20)
                .with_flags(IS_DELETED),
            file(22, 20, "kept.bin").sized(1 << 10),
        ]);
        let tree = Tree::build(&index);
        assert_eq!(tree.totals(0).files, 1);
        assert_eq!(tree.totals(0).allocated, 4096);
        assert!(!tree.contains(1));
    }

    #[test]
    fn a_cycle_cut_off_from_the_root_is_simply_absent() {
        let index = index_from_specs(vec![
            dir(20, 21, "a"),
            dir(21, 20, "b"),
            file(22, ROOT_RECORD, "ok.txt").sized(10),
        ]);
        let tree = Tree::build(&index);
        assert_eq!(tree.totals(tree.root()).files, 1);
        assert_eq!(tree.totals(tree.root()).dirs, 0);
        assert!(!tree.contains(0));
        assert!(!tree.contains(1));
        assert_eq!(tree.heaviest_folders(5).len(), 1);
    }

    #[test]
    fn the_root_record_itself_is_folded_into_the_synthetic_root() {
        // A scan with metafiles included carries record 5, whose parent is 5.
        let index = index_from_specs(vec![
            dir(ROOT_RECORD, ROOT_RECORD, "."),
            file(20, ROOT_RECORD, "a.txt").sized(10),
        ]);
        let tree = Tree::build(&index);
        assert_eq!(tree.children(tree.root()), &[1]);
        assert_eq!(tree.totals(tree.root()).dirs, 0);
    }

    #[test]
    fn a_file_whose_parent_is_a_file_is_not_placed() {
        // Only possible on a damaged volume, but must not panic or count.
        let index = index_from_specs(vec![
            file(20, ROOT_RECORD, "a.txt").sized(10),
            file(21, 20, "b.txt").sized(10),
        ]);
        let tree = Tree::build(&index);
        assert_eq!(tree.totals(tree.root()).files, 1);
    }

    #[test]
    fn largest_files_and_heaviest_folders() {
        let index = sample();
        let tree = Tree::build(&index);
        let top: Vec<_> = tree
            .largest_files(2)
            .iter()
            .map(|n| index.name(*n as usize).to_string())
            .collect();
        assert_eq!(top, ["movie.mkv", "pagefile.sys"]);

        let heavy: Vec<_> = tree
            .heaviest_folders(2)
            .iter()
            .map(|(n, _)| tree.name(&index, *n).into_owned())
            .collect();
        // `pc` holds the movie itself; `Users` only holds `pc`.
        assert_eq!(heavy, ["pc", "C:\\"]);
    }

    #[test]
    fn ancestry_runs_from_the_root() {
        let index = sample();
        let tree = Tree::build(&index);
        assert_eq!(tree.ancestry(3), vec![tree.root(), 0, 1, 3]);
        assert_eq!(tree.path(&index, 3), r"C:\Users\pc\movie.mkv");
    }

    #[test]
    fn extensions_are_grouped_case_insensitively() {
        let index = index_from_specs(vec![
            file(20, ROOT_RECORD, "a.LOG").sized(4096),
            file(21, ROOT_RECORD, "b.log").sized(4096),
            file(22, ROOT_RECORD, "README").sized(10),
            dir(23, ROOT_RECORD, "x").with_flags(IS_DIR),
        ]);
        let tree = Tree::build(&index);
        let exts = tree.extensions_under(&index, tree.root());
        assert_eq!(exts[0].0, "log");
        assert_eq!(exts[0].1.files, 2);
        assert!(exts.iter().any(|(e, _)| e.is_empty()));
    }
}
