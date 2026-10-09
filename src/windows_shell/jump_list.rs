//! The taskbar's jump list: the tasks, and the files used recently, kept in
//! `recent.json` in jot's settings folder.

use super::TASKS;
use super::recent::RecentFiles;
use crate::actions::{NewTab, NewWindow};
use crate::instance::Kind;
use gpui_kit::{App, Global, MenuItem};
use std::path::{Path, PathBuf};

/// Notes that the file at `path` was opened or saved, for the jump list.
pub fn file_used(path: &Path, cx: &mut App) {
    cx.add_recent_document(path);
    edit_recent(|recent| {
        recent.add(path);
        true
    });
    refresh(cx);
}

/// Whether the jump list is being updated, and whether it changed since.
#[derive(Default)]
struct Updating {
    running: bool,
    again: bool,
}

impl Global for Updating {}

/// Updates the jump list from the recent files, leaving out those that are
/// gone and those the user removed from it. One update runs at a time, and
/// another follows when the files change meanwhile.
pub fn refresh(cx: &mut App) {
    let updating = cx.default_global::<Updating>();
    if updating.running {
        updating.again = true;
        return;
    }
    updating.running = true;
    cx.spawn(async move |cx| {
        loop {
            // A file on a network share can take a while to look for.
            let files = load().files().to_vec();
            let gone: Vec<PathBuf> = cx
                .background_executor()
                .spawn(async move { files.into_iter().filter(|file| !file.is_file()).collect() })
                .await;
            let removed = cx
                .update(|cx| {
                    let recent = edit_recent(|recent| recent.forget(&gone));
                    let entries = recent
                        .files()
                        .iter()
                        .map(|file| vec![file.clone()].into())
                        .collect();
                    cx.update_jump_list(tasks(), entries)
                })
                .await;
            let removed: Vec<PathBuf> = removed.into_iter().flatten().collect();
            edit_recent(|recent| recent.forget(&removed));

            let again = cx.update(|cx| {
                let updating = cx.global_mut::<Updating>();
                updating.running = std::mem::take(&mut updating.again);
                updating.running
            });
            if !again {
                break;
            }
        }
    })
    .detach();
}

/// The tasks, as GPUI takes them. Their actions name what they do, and the
/// launch that a task starts carries it out.
fn tasks() -> Vec<MenuItem> {
    TASKS
        .iter()
        .map(|(name, kind)| match kind {
            Kind::Open => MenuItem::action(*name, NewWindow),
            Kind::NewDocument => MenuItem::action(*name, NewTab),
        })
        .collect()
}

fn recent_path() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("jot").join("recent.json"))
}

fn load() -> RecentFiles {
    recent_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|json| RecentFiles::from_json(&json))
        .unwrap_or_default()
}

/// Changes the saved list with `change`, which returns whether it changed
/// anything, and returns the list. It is read each time, since a jot started
/// with `--new-instance` keeps it too.
fn edit_recent(change: impl FnOnce(&mut RecentFiles) -> bool) -> RecentFiles {
    let mut recent = load();
    if change(&mut recent)
        && let Some(path) = recent_path()
    {
        let saved = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, recent.to_json()));
        if let Err(error) = saved {
            log::error!("Couldn\u{2019}t save the recent files: {error}");
        }
    }
    recent
}
