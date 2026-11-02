//! The files opened or saved in jot most recently, which the taskbar's jump
//! list shows.

use super::same_path;
use gpui_kit::{JumpListIcon, JumpListRecent};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

/// How many files the list keeps.
pub const MAX_FILES: usize = 10;

/// The recent files, the most recent first, saved as JSON.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecentFiles {
    files: Vec<PathBuf>,
}

impl RecentFiles {
    /// Reads a list saved by [`Self::to_json`]. One that can't be read is
    /// empty.
    pub fn from_json(json: &str) -> Self {
        let mut recent: Self = serde_json::from_str(json).unwrap_or_default();
        recent.files.truncate(MAX_FILES);
        recent
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }

    /// Puts `path` first. A path that isn't Unicode is left out, since the
    /// list is saved as text.
    pub fn add(&mut self, path: &Path) {
        if path.to_str().is_none() {
            return;
        }
        self.files.retain(|file| !same_path(file, path));
        self.files.insert(0, path.to_path_buf());
        self.files.truncate(MAX_FILES);
    }

    /// Takes `paths` off the list, and returns whether any were on it.
    pub fn forget(&mut self, paths: &[PathBuf]) -> bool {
        let before = self.files.len();
        self.files
            .retain(|file| !paths.iter().any(|path| same_path(file, path)));
        self.files.len() != before
    }

    /// The files as the jump list's Recent category, each with jot's icon.
    /// Choosing one opens it in jot.
    pub fn jump_list(&self) -> JumpListRecent {
        JumpListRecent {
            title: "Recent".into(),
            entries: self
                .files
                .iter()
                .map(|file| vec![file.clone()].into())
                .collect(),
            icon: JumpListIcon::App,
            // Windows' own list would show the same files again.
            system_recent: false,
        }
    }
}

/// Changes the list saved in the file at `file` with `change`, which says
/// whether it changed anything, and returns the list. The file is read and
/// written in one save, so that a change by another jot comes before or
/// after this one, never between its read and its write. A copy of the file
/// goes to `backups` while it's written in place.
pub fn edit_saved(
    file: &Path,
    backups: &Path,
    mut change: impl FnMut(&mut RecentFiles) -> bool,
) -> io::Result<RecentFiles> {
    if let Some(folder) = file.parent() {
        std::fs::create_dir_all(folder)?;
    }
    let mut recent = RecentFiles::default();
    crate::file_write::update_whole(file, backups, |saved| {
        recent = saved
            .map(|json| RecentFiles::from_json(&String::from_utf8_lossy(&json)))
            .unwrap_or_default();
        change(&mut recent).then(|| recent.to_json().into_bytes())
    })?;
    Ok(recent)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(name: &str) -> PathBuf {
        PathBuf::from(format!(r"C:\notes\{name}"))
    }

    #[test]
    fn the_most_recent_file_comes_first_once() {
        let mut recent = RecentFiles::default();
        recent.add(&path("a.txt"));
        recent.add(&path("b.md"));
        recent.add(&path("A.TXT"));
        assert_eq!(recent.files(), [path("A.TXT"), path("b.md")]);
    }

    #[test]
    fn the_list_keeps_the_ten_most_recent() {
        let mut recent = RecentFiles::default();
        for n in 0..15 {
            recent.add(&path(&format!("{n}.txt")));
        }
        assert_eq!(recent.files().len(), MAX_FILES);
        assert_eq!(recent.files()[0], path("14.txt"));
        assert_eq!(recent.files()[9], path("5.txt"));
    }

    #[test]
    fn forgotten_files_leave_the_list() {
        let mut recent = RecentFiles::default();
        for name in ["a.txt", "b.txt", "c.txt"] {
            recent.add(&path(name));
        }
        assert!(recent.forget(&[path("B.txt"), path("missing.txt")]));
        assert_eq!(recent.files(), [path("c.txt"), path("a.txt")]);
        assert!(!recent.forget(&[path("missing.txt")]));
    }

    #[test]
    fn the_list_round_trips_and_a_broken_one_is_empty() {
        let mut recent = RecentFiles::default();
        recent.add(&path("a b.txt"));
        recent.add(&path("café.md"));
        assert_eq!(RecentFiles::from_json(&recent.to_json()), recent);
        assert_eq!(RecentFiles::from_json("{not json"), RecentFiles::default());
        assert_eq!(RecentFiles::from_json("{}"), RecentFiles::default());
    }

    #[test]
    fn the_jump_list_shows_the_files_as_recent_with_jots_icon() {
        let mut recent = RecentFiles::default();
        recent.add(&path("a.txt"));
        recent.add(&path("b.md"));
        let category = recent.jump_list();
        assert_eq!(category.title, "Recent");
        assert_eq!(category.icon, JumpListIcon::App);
        assert!(!category.system_recent);
        let entries: Vec<Vec<PathBuf>> = category
            .entries
            .iter()
            .map(|entry| entry.to_vec())
            .collect();
        assert_eq!(entries, [vec![path("b.md")], vec![path("a.txt")]]);
    }

    #[cfg(unix)]
    #[test]
    fn a_path_that_is_not_unicode_is_left_out() {
        use std::os::unix::ffi::OsStringExt;
        let mut recent = RecentFiles::default();
        recent.add(&PathBuf::from(std::ffi::OsString::from_vec(
            b"/notes/caf\xe9.txt".to_vec(),
        )));
        assert!(recent.files().is_empty());
    }

    /// Files added by several jots at once all go in.
    #[test]
    fn edits_from_several_jots_at_once_all_go_in() {
        let folder = std::env::temp_dir().join(format!("jot-recent-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        let file = folder.join("jot").join("recent.json");
        let backups = folder.join("backups");
        let edits: Vec<_> = (0..4)
            .map(|jot| {
                let (file, backups) = (file.clone(), backups.clone());
                std::thread::spawn(move || {
                    for n in 0..2 {
                        edit_saved(&file, &backups, |recent| {
                            recent.add(&path(&format!("{jot}-{n}.txt")));
                            std::thread::yield_now();
                            true
                        })
                        .unwrap();
                    }
                })
            })
            .collect();
        for edit in edits {
            edit.join().unwrap();
        }
        let saved = RecentFiles::from_json(&std::fs::read_to_string(&file).unwrap());
        assert_eq!(saved.files().len(), 8, "{saved:?}");

        // An edit that changes nothing returns the list and writes nothing.
        let before = std::fs::metadata(&file).unwrap().modified().unwrap();
        let recent = edit_saved(&file, &backups, |_| false).unwrap();
        assert_eq!(recent, saved);
        assert_eq!(
            std::fs::metadata(&file).unwrap().modified().unwrap(),
            before
        );
        std::fs::remove_dir_all(&folder).unwrap();
    }
}
