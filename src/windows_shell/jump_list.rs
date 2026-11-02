//! The taskbar's jump list: the tasks, and the files used recently, kept in
//! `recent.json` in jot's settings folder.

use super::TASKS;
use super::recent::edit_saved;
use crate::actions::{NewTab, NewWindow};
use crate::instance::Kind;
use crate::state::{backup_folder, on_own_thread};
use gpui_kit::{App, Global, MenuItem};
use std::path::{Path, PathBuf};

/// Notes that the file at `path` was opened or saved, for the jump list and
/// Windows' recent items.
pub fn file_used(path: &Path, cx: &mut App) {
    cx.add_recent_document(path);
    cx.default_global::<Updating>()
        .used
        .push(path.to_path_buf());
    refresh(cx);
}

/// Where the recent files are kept, and where a copy of that file goes
/// while it's written in place. Tests keep theirs elsewhere.
struct RecentFile {
    file: Option<PathBuf>,
    backups: PathBuf,
}

impl Default for RecentFile {
    fn default() -> Self {
        Self {
            // Tests keep the user's list as it is.
            file: dirs::config_dir()
                .filter(|_| !cfg!(test))
                .map(|dir| dir.join("jot").join("recent.json")),
            backups: backup_folder(),
        }
    }
}

impl Global for RecentFile {}

/// Whether the jump list is being updated, whether it changed since, and
/// the files used since the last update.
#[derive(Default)]
struct Updating {
    running: bool,
    again: bool,
    used: Vec<PathBuf>,
}

impl Global for Updating {}

/// Updates the recent files with those used since, and the jump list from
/// them, leaving out files that are gone and those the user removed from
/// it. One update runs at a time, and another follows when files are used
/// meanwhile. The file is read and written on a thread of its own, since
/// another jot may hold it and a file on a network share can take a while
/// to look for.
pub fn refresh(cx: &mut App) {
    let updating = cx.default_global::<Updating>();
    if updating.running {
        updating.again = true;
        return;
    }
    updating.running = true;
    let recent_file = cx.default_global::<RecentFile>();
    let (file, backups) = (recent_file.file.clone(), recent_file.backups.clone());
    cx.spawn(async move |cx| {
        // Without a settings folder there's no list to keep.
        let Some(file) = file else {
            cx.update(|cx| *cx.global_mut::<Updating>() = Updating::default());
            return;
        };
        loop {
            let used = cx.update(|cx| std::mem::take(&mut cx.global_mut::<Updating>().used));
            let (file_now, backups_now) = (file.clone(), backups.clone());
            let recent = on_own_thread(move || {
                let (file, backups) = (file_now, backups_now);
                let recent = edit_saved(&file, &backups, |recent| {
                    for path in &used {
                        recent.add(path);
                    }
                    !used.is_empty()
                })?;
                let gone: Vec<PathBuf> = recent
                    .files()
                    .iter()
                    .filter(|file| !file.is_file())
                    .cloned()
                    .collect();
                if gone.is_empty() {
                    return Ok(recent);
                }
                edit_saved(&file, &backups, |recent| recent.forget(&gone))
            })
            .await;
            match recent {
                Ok(recent) => {
                    let removed = cx
                        .update(|cx| cx.update_jump_list_with(tasks(), recent.jump_list()))
                        .await;
                    let removed: Vec<PathBuf> = removed.into_iter().flatten().collect();
                    if !removed.is_empty() {
                        let (file, backups) = (file.clone(), backups.clone());
                        let forgotten = on_own_thread(move || {
                            edit_saved(&file, &backups, |recent| recent.forget(&removed))
                        })
                        .await;
                        if let Err(error) = forgotten {
                            log::error!("Couldn\u{2019}t save the recent files. {error}");
                        }
                    }
                }
                Err(error) => log::error!("Couldn\u{2019}t save the recent files. {error}"),
            }

            let again = cx.update(|cx| {
                let updating = cx.global_mut::<Updating>();
                updating.running = std::mem::take(&mut updating.again);
                updating.running
            });
            if !again {
                return;
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

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::TestAppContext;
    use std::io::Read as _;
    use std::time::{Duration, Instant};

    /// While another jot holds the recent files, the window goes on, and
    /// the file used goes in once that jot lets go.
    #[gpui_kit::test]
    fn a_held_recent_files_file_doesnt_hold_up_the_window(cx: &mut TestAppContext) {
        let folder =
            std::env::temp_dir().join(format!("jot-jump-list-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        let file = folder.join("recent.json");
        std::fs::write(&file, "{}").unwrap();
        let used = folder.join("notes.txt");
        std::fs::write(&used, "").unwrap();
        cx.update(|cx| {
            cx.set_global(RecentFile {
                file: Some(file.clone()),
                backups: folder.join("backups"),
            })
        });
        // The update ends on a thread of its own, which wakes the test's.
        cx.executor().allow_parking();

        let mut held = std::fs::File::open(&file).unwrap();
        held.lock().unwrap();
        cx.update(|cx| file_used(&used, cx));
        cx.run_until_parked();
        let mut saved = String::new();
        held.read_to_string(&mut saved).unwrap();
        assert_eq!(saved, "{}");
        drop(held);

        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            let saved = std::fs::read_to_string(&file).unwrap_or_default();
            if saved.contains("notes.txt") {
                break;
            }
            assert!(Instant::now() < deadline, "the file used never went in");
            std::thread::sleep(Duration::from_millis(10));
        }
        std::fs::remove_dir_all(&folder).unwrap();
    }
}
