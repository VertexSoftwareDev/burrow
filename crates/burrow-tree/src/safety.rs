//! What may be removed from inside Burrow, decided in one place.
//!
//! A person who does not know what `NTUSER.DAT` or `AppData\Local\Programs`
//! is must not be able to break their computer from here — not by a slip,
//! not by trusting a suggestion, not by a "keep one copy" button. So every
//! removal, from every part of the window, asks [`Policy::check`], and the
//! worker asks again at the last moment before anything is moved.
//!
//! The rule is conservative on purpose. What is allowed is the person's own
//! files in their own profile — Documents, Downloads, Desktop, Pictures,
//! Videos, Music, OneDrive, and folders they made — plus folders at the top
//! of the drive that hold no programs. Everything else is refused, with a
//! reason the window can say out loud:
//!
//! - the drive's root and the files in it; Windows, boot and recovery;
//! - installed programs: Program Files, ProgramData, `AppData\Local\Programs`,
//!   and any top-level folder that holds executables;
//! - the skeleton of a profile: `Users`, each profile folder, its loose files
//!   (`NTUSER.DAT`), `Desktop`, `Documents` and the other shell folders
//!   themselves — their contents are fine, the folders are not;
//! - other people's profiles;
//! - application data: `AppData` and dot-folders such as `.minecraft`,
//!   `.vscode`, `.ssh`. Applications read these from exact paths; identical
//!   bytes elsewhere do not help them. The one exception is the temporary
//!   folder's contents;
//! - anything Windows itself marks as a system file, and links (junctions,
//!   symlinks), whose removal does not mean what it looks like.
//!
//! The cleanup rules reach into a few refused places on purpose — Windows'
//! temp folder, browser caches in AppData — through explicit, reviewed
//! paths. Those are marked trusted; nothing else is.

use std::cell::RefCell;
use std::collections::HashMap;

use ferret_core::Index;

use crate::{Kind, NodeId, Tree};

/// Why something may not be removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Block {
    /// The drive itself, or a file lying in its root.
    Root,
    /// Windows, boot, recovery, the Recycle Bin, NTFS's own files.
    System,
    /// Installed programs.
    Programs,
    /// `Users`, a profile folder, its loose files, or a shell folder itself.
    Profile,
    /// Someone else's profile.
    OtherUser,
    /// An application's own data.
    AppData,
    /// Marked as a system file by Windows.
    SystemFile,
    /// A junction or symbolic link.
    Link,
}

/// Top-level folders that belong to Windows.
const SYSTEM_TOP: &[&str] = &[
    "windows",
    "system volume information",
    "recovery",
    "boot",
    "efi",
    "perflogs",
    "msocache",
    "config.msi",
    "documents and settings",
    "windows.old",
    "onedrivetemp",
];

const PROGRAM_TOP: &[&str] = &["program files", "program files (x86)", "programdata"];

/// Profiles no person logs into.
const SHARED_PROFILES: &[&str] = &["default", "default user", "public", "all users"];

/// Folders directly in a profile that Windows and Explorer expect to exist.
const SHELL_FOLDERS: &[&str] = &[
    "appdata",
    "desktop",
    "documents",
    "downloads",
    "pictures",
    "music",
    "videos",
    "favorites",
    "contacts",
    "links",
    "saved games",
    "searches",
    "3d objects",
    "onedrive",
];

/// Decides removals for one scan. Cheap to build; remembers which top-level
/// folders hold programs, since that takes a walk to find out.
pub struct Policy<'a> {
    index: &'a Index,
    tree: &'a Tree,
    /// The signed-in person's profile folder name, lowercased.
    profile: String,
    programs_at_top: RefCell<HashMap<NodeId, bool>>,
}

impl<'a> Policy<'a> {
    pub fn new(index: &'a Index, tree: &'a Tree, profile: &str) -> Self {
        Self {
            index,
            tree,
            profile: profile.to_lowercase(),
            programs_at_top: RefCell::new(HashMap::new()),
        }
    }

    fn name(&self, node: NodeId) -> String {
        self.index.name(node as usize).to_lowercase()
    }

    /// `Ok` when `node` may be moved to the Recycle Bin by the person.
    pub fn check(&self, node: NodeId) -> Result<(), Block> {
        let tree = self.tree;
        if node == tree.root() || !tree.contains(node) {
            return Err(Block::Root);
        }
        let chain = tree.ancestry(node);
        let top = chain[1];
        let top_name = self.name(top);
        let is_dir = tree.is_dir(node);

        if chain.len() == 2 && !is_dir {
            return Err(Block::Root);
        }
        if top_name.starts_with('$') || SYSTEM_TOP.contains(&top_name.as_str()) {
            return Err(Block::System);
        }
        if PROGRAM_TOP.contains(&top_name.as_str()) {
            return Err(Block::Programs);
        }

        if top_name == "users" {
            self.check_profile(&chain)?;
        } else if self.holds_programs(top) {
            // C:\Java, C:\Python313, C:\Games\Something: installed software
            // is removed with its uninstaller, not a file manager.
            return Err(Block::Programs);
        }

        let entry = &self.index.entries()[node as usize];
        if entry.is_system() {
            return Err(Block::SystemFile);
        }
        if entry.is_reparse() && !entry.is_cloud() {
            return Err(Block::Link);
        }
        Ok(())
    }

    fn check_profile(&self, chain: &[NodeId]) -> Result<(), Block> {
        // chain: root, Users, <profile>, ...
        if chain.len() <= 3 {
            return Err(Block::Profile);
        }
        let profile = self.name(chain[2]);
        if SHARED_PROFILES.contains(&profile.as_str()) {
            return Err(Block::Profile);
        }
        if profile != self.profile {
            return Err(Block::OtherUser);
        }
        let first = self.name(chain[3]);
        let node = *chain.last().unwrap_or(&chain[3]);
        if chain.len() == 4 {
            // Directly in the profile: its own files and its shell folders.
            if !self.tree.is_dir(node)
                || SHELL_FOLDERS.contains(&first.as_str())
                || first.starts_with("onedrive")
            {
                return Err(Block::Profile);
            }
        }
        if first == "appdata" {
            // AppData\Local\Temp's contents are the one part that is
            // everybody's to clear; the folder itself stays.
            let second = chain.get(4).map(|n| self.name(*n));
            let third = chain.get(5).map(|n| self.name(*n));
            let in_temp = second.as_deref() == Some("local")
                && third.as_deref() == Some("temp")
                && chain.len() > 6;
            if !in_temp {
                return Err(Block::AppData);
            }
        }
        if chain[3..].iter().any(|n| {
            let name = self.name(*n);
            name.starts_with('.') && name.len() > 1
        }) {
            return Err(Block::AppData);
        }
        Ok(())
    }

    /// Whether a top-level folder holds programs anywhere inside.
    fn holds_programs(&self, top: NodeId) -> bool {
        if let Some(known) = self.programs_at_top.borrow().get(&top) {
            return *known;
        }
        let tree = self.tree;
        let mut stack = vec![top];
        let mut found = false;
        'walk: while let Some(node) = stack.pop() {
            for &child in tree.children(node) {
                if tree.is_dir(child) {
                    stack.push(child);
                } else if Kind::of(self.index.name(child as usize)) == Kind::Executable {
                    found = true;
                    break 'walk;
                }
            }
        }
        self.programs_at_top.borrow_mut().insert(top, found);
        found
    }

    pub fn allows(&self, node: NodeId) -> bool {
        self.check(node).is_ok()
    }

    /// The last word before anything moves. `trusted` is set only for what
    /// a reviewed cleanup path named: that may reach into application data,
    /// Windows' temp folders and system files — but never the root, a
    /// profile's skeleton, someone else's profile, or through a link.
    pub fn may_remove(&self, node: NodeId, trusted: bool) -> bool {
        match self.check(node) {
            Ok(()) => true,
            Err(Block::AppData | Block::System | Block::Programs | Block::SystemFile) => trusted,
            Err(Block::Root | Block::Profile | Block::OtherUser | Block::Link) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferret_core::mft::{IS_REPARSE, IS_SYSTEM};
    use ferret_core::testing::{dir, file, index_from_specs};
    use ferret_core::ROOT_RECORD;

    fn sample() -> (Index, Tree) {
        let index = index_from_specs(vec![
            dir(20, ROOT_RECORD, "Users"),
            dir(21, 20, "pc"),
            file(22, 21, "NTUSER.DAT").sized(8 << 20),
            dir(23, 21, "Documents"),
            file(24, 23, "report.pdf").sized(1 << 20),
            dir(25, 21, "AppData"),
            dir(26, 25, "Local"),
            dir(27, 26, "Programs"),
            dir(28, 27, "LM Studio"),
            file(29, 28, "lms.exe").sized(100 << 20),
            dir(30, 26, "Temp"),
            file(31, 30, "junk.tmp").sized(1 << 20),
            dir(32, 21, ".minecraft"),
            file(33, 32, "client.jar").sized(30 << 20),
            dir(34, 21, "Projects"),
            file(35, 34, "notes.txt").sized(10),
            file(36, 34, "desktop.ini").sized(10).with_flags(IS_SYSTEM),
            dir(37, 34, "link").with_flags(IS_REPARSE),
            dir(40, 20, "someone"),
            file(41, 40, "their.txt").sized(10),
            dir(42, 20, "Public"),
            file(43, 42, "shared.txt").sized(10),
            dir(50, ROOT_RECORD, "Windows"),
            file(51, 50, "explorer.exe").sized(5 << 20),
            dir(52, ROOT_RECORD, "Program Files"),
            file(53, 52, "app.exe").sized(5 << 20),
            dir(54, ROOT_RECORD, "Java"),
            dir(55, 54, "bin"),
            file(56, 55, "java.exe").sized(1 << 20),
            dir(57, ROOT_RECORD, "Filmler"),
            file(58, 57, "tatil.mp4").sized(1 << 30),
            file(59, ROOT_RECORD, "pagefile.sys").sized(9 << 30),
            dir(60, ROOT_RECORD, "$Recycle.Bin"),
        ]);
        let tree = Tree::build(&index);
        (index, tree)
    }

    fn verdict(index: &Index, tree: &Tree, name: &str) -> Result<(), Block> {
        let node = (0..tree.root())
            .find(|n| index.name(*n as usize) == name)
            .unwrap_or_else(|| panic!("no {name}"));
        Policy::new(index, tree, "pc").check(node)
    }

    #[test]
    fn a_persons_own_files_may_go() {
        let (index, tree) = sample();
        assert_eq!(verdict(&index, &tree, "report.pdf"), Ok(()));
        assert_eq!(verdict(&index, &tree, "Projects"), Ok(()));
        assert_eq!(verdict(&index, &tree, "notes.txt"), Ok(()));
        assert_eq!(verdict(&index, &tree, "junk.tmp"), Ok(()));
        // A top-level folder of films holds no programs.
        assert_eq!(verdict(&index, &tree, "Filmler"), Ok(()));
        assert_eq!(verdict(&index, &tree, "tatil.mp4"), Ok(()));
    }

    #[test]
    fn the_system_and_programs_may_not() {
        let (index, tree) = sample();
        assert_eq!(verdict(&index, &tree, "explorer.exe"), Err(Block::System));
        assert_eq!(verdict(&index, &tree, "$Recycle.Bin"), Err(Block::System));
        assert_eq!(verdict(&index, &tree, "app.exe"), Err(Block::Programs));
        assert_eq!(verdict(&index, &tree, "pagefile.sys"), Err(Block::Root));
        // C:\Java holds java.exe somewhere inside: an installed program.
        assert_eq!(verdict(&index, &tree, "Java"), Err(Block::Programs));
        assert_eq!(verdict(&index, &tree, "java.exe"), Err(Block::Programs));
    }

    #[test]
    fn the_profile_skeleton_and_other_people_may_not() {
        let (index, tree) = sample();
        assert_eq!(verdict(&index, &tree, "Users"), Err(Block::Profile));
        assert_eq!(verdict(&index, &tree, "pc"), Err(Block::Profile));
        assert_eq!(verdict(&index, &tree, "NTUSER.DAT"), Err(Block::Profile));
        assert_eq!(verdict(&index, &tree, "Documents"), Err(Block::Profile));
        assert_eq!(verdict(&index, &tree, "their.txt"), Err(Block::OtherUser));
        assert_eq!(verdict(&index, &tree, "shared.txt"), Err(Block::Profile));
    }

    #[test]
    fn application_data_may_not_except_temp_contents() {
        let (index, tree) = sample();
        assert_eq!(verdict(&index, &tree, "lms.exe"), Err(Block::AppData));
        assert_eq!(verdict(&index, &tree, "client.jar"), Err(Block::AppData));
        assert_eq!(verdict(&index, &tree, ".minecraft"), Err(Block::AppData));
        assert_eq!(verdict(&index, &tree, "Temp"), Err(Block::AppData));
        assert_eq!(verdict(&index, &tree, "junk.tmp"), Ok(()));
    }

    #[test]
    fn system_files_and_links_may_not() {
        let (index, tree) = sample();
        assert_eq!(
            verdict(&index, &tree, "desktop.ini"),
            Err(Block::SystemFile)
        );
        assert_eq!(verdict(&index, &tree, "link"), Err(Block::Link));
    }

    #[test]
    fn the_profile_name_is_matched_without_case() {
        let (index, tree) = sample();
        let node = (0..tree.root())
            .find(|n| index.name(*n as usize) == "notes.txt")
            .unwrap();
        assert_eq!(Policy::new(&index, &tree, "PC").check(node), Ok(()));
        assert_eq!(
            Policy::new(&index, &tree, "other").check(node),
            Err(Block::OtherUser)
        );
    }
}
