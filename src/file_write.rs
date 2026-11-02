//! Saving a file whole, so that a save that fails leaves the file as it was.
//!
//! The new contents go to a temporary file beside the old one, which then
//! takes its place, so the file holds the old contents or the new and never
//! part of either. A save through a symbolic link writes the file the link
//! points to and leaves the link. The file keeps its permissions and, where
//! the system allows, its owner.
//!
//! A file with other names (hard links) would lose them to a new file, and
//! so would a file whose owner the new one can't take. Such a file is
//! written in place instead, after a copy of it is made beside it. If the
//! write fails partway, the copy is put back.

use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Replaces the contents of the file at `path` with `data`, or makes the
/// file. The folder must exist.
pub fn write_whole(path: &Path, data: &[u8]) -> io::Result<()> {
    write_whole_with(path, &mut |file| file.write_all(data))
}

/// [`write_whole`], with `write` writing the new contents to the file it's
/// given. It's called once.
fn write_whole_with(
    path: &Path,
    write: &mut dyn FnMut(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    let target = follow_links(path)?;
    // Opening the file for writing fails as writing it would, for a
    // read-only file or one another program holds, so that replacing it
    // doesn't get around that.
    let original = match OpenOptions::new().read(true).write(true).open(&target) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return write_and_replace(&target, None, write).map(|_| ());
        }
        Err(error) => return Err(error),
    };
    if link_count(&original)? > 1 {
        return write_in_place(&target, original, write);
    }
    let metadata = original.metadata()?;
    // Closed first, so that Windows lets the new file take its place.
    drop(original);
    if write_and_replace(&target, Some(&metadata), write)? {
        return Ok(());
    }
    let original = OpenOptions::new().read(true).write(true).open(&target)?;
    write_in_place(&target, original, write)
}

/// `path`, or the file its symbolic links lead to.
fn follow_links(path: &Path) -> io::Result<PathBuf> {
    let mut path = path.to_path_buf();
    // As many links as Linux follows.
    for _ in 0..40 {
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let link = fs::read_link(&path)?;
                path = match path.parent() {
                    Some(folder) => folder.join(link),
                    None => link,
                };
            }
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => return Ok(path),
        }
    }
    Err(io::Error::other(format!(
        "{} is a symbolic link in a loop",
        path.display()
    )))
}

/// Writes a temporary file beside `target` and renames it over `target`.
/// The new file takes `original`'s owner and permissions. Returns `false`,
/// having changed nothing, when it can't take the owner.
fn write_and_replace(
    target: &Path,
    original: Option<&Metadata>,
    write: &mut dyn FnMut(&mut File) -> io::Result<()>,
) -> io::Result<bool> {
    let (temporary_path, mut temporary) = create_temporary(target)?;
    let written = (|| {
        if let Some(original) = original {
            if !take_owner(&temporary, original) {
                return Ok(false);
            }
            // After the owner, since changing the owner can clear the setuid
            // and setgid bits.
            temporary.set_permissions(original.permissions())?;
        }
        write(&mut temporary)?;
        temporary.sync_all()?;
        Ok(true)
    })();
    drop(temporary);
    let replaced = written.and_then(|written| {
        if written {
            replace(&temporary_path, target)?;
            sync_folder(target);
        }
        Ok(written)
    });
    if !matches!(replaced, Ok(true)) {
        let _ = fs::remove_file(&temporary_path);
    }
    replaced
}

/// Writes `file`, which is `target`, in place, after copying it beside
/// itself. The copy is put back if the write fails.
fn write_in_place(
    target: &Path,
    mut file: File,
    write: &mut dyn FnMut(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    let (backup_path, mut backup) = create_temporary(target)?;
    let copied = (|| {
        // Readable only by whom the file is.
        backup.set_permissions(file.metadata()?.permissions())?;
        io::copy(&mut file, &mut backup)?;
        backup.sync_all()
    })();
    if let Err(error) = copied {
        drop(backup);
        let _ = fs::remove_file(&backup_path);
        return Err(error);
    }
    let Err(error) = overwrite(&mut file, write) else {
        drop(backup);
        let _ = fs::remove_file(&backup_path);
        return Ok(());
    };
    let restored = backup
        .seek(SeekFrom::Start(0))
        .and_then(|_| overwrite(&mut file, &mut |file| io::copy(&mut backup, file).map(drop)));
    drop(backup);
    match restored {
        Ok(()) => {
            let _ = fs::remove_file(&backup_path);
            Err(error)
        }
        Err(_) => Err(io::Error::new(
            error.kind(),
            format!(
                "{error}. The file as it was is in {}.",
                backup_path.display()
            ),
        )),
    }
}

/// Writes `file` from its start with `write`, and ends it there.
fn overwrite(
    file: &mut File,
    write: &mut dyn FnMut(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    file.seek(SeekFrom::Start(0))?;
    write(file)?;
    let end = file.stream_position()?;
    file.set_len(end)?;
    file.sync_all()
}

/// A new file beside `target`, named after it and hidden on Unix.
fn create_temporary(target: &Path) -> io::Result<(PathBuf, File)> {
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    for attempt in 0.. {
        let path = target.with_file_name(format!(".{name}.{}-{attempt}.tmp", std::process::id()));
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
        {
            // One left behind by an earlier jot with the same process id.
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists && attempt < 100 => {}
            opened => return opened.map(|file| (path, file)),
        }
    }
    unreachable!()
}

/// Gives `file` the owner and group of `original`, and returns whether it
/// has them. Only the superuser may give a file away, while anyone may give
/// one of their own files to a group they belong to.
#[cfg(unix)]
fn take_owner(file: &File, original: &Metadata) -> bool {
    use std::os::unix::fs::{MetadataExt, fchown};
    let owner = (original.uid(), original.gid());
    let has_owner = |file: &File| {
        file.metadata()
            .is_ok_and(|metadata| (metadata.uid(), metadata.gid()) == owner)
    };
    has_owner(file)
        || fchown(file, Some(owner.0), Some(owner.1)).is_ok()
        || (fchown(file, None, Some(owner.1)).is_ok() && has_owner(file))
}

#[cfg(not(unix))]
fn take_owner(_: &File, _: &Metadata) -> bool {
    true
}

/// How many names the file has.
#[cfg(unix)]
fn link_count(file: &File) -> io::Result<u64> {
    use std::os::unix::fs::MetadataExt;
    Ok(file.metadata()?.nlink())
}

#[cfg(windows)]
fn link_count(file: &File) -> io::Result<u64> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the handle is the open file's, and `info` outlives the call.
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }?;
    Ok(info.nNumberOfLinks.into())
}

#[cfg(not(any(unix, windows)))]
fn link_count(_: &File) -> io::Result<u64> {
    Ok(1)
}

/// Renames `from` over `to`. On Windows another program, such as a virus
/// scanner, may hold either file for a moment, so a refusal is tried again,
/// for about 600 ms in all.
fn replace(from: &Path, to: &Path) -> io::Result<()> {
    const WAITS_MS: [u64; 6] = [10, 20, 40, 80, 160, 320];
    let mut waits = WAITS_MS.iter();
    loop {
        match fs::rename(from, to) {
            Err(error) if is_held_elsewhere(&error) => match waits.next() {
                Some(&wait) => std::thread::sleep(Duration::from_millis(wait)),
                None => return Err(error),
            },
            result => return result,
        }
    }
}

/// Whether Windows refused a rename because another program has the file
/// open, which shows as access denied, a sharing violation or a lock
/// violation.
fn is_held_elsewhere(error: &io::Error) -> bool {
    cfg!(windows) && matches!(error.raw_os_error(), Some(5 | 32 | 33))
}

/// Makes the rename of a file in `target`'s folder last through a power cut.
/// The save is done by then, and some file systems can't sync a folder, so a
/// failure is ignored. Windows syncs no folders.
fn sync_folder(target: &Path) {
    #[cfg(unix)]
    {
        let folder = match target.parent() {
            Some(folder) if !folder.as_os_str().is_empty() => folder,
            _ => Path::new("."),
        };
        if let Ok(folder) = File::open(folder) {
            let _ = folder.sync_all();
        }
    }
    #[cfg(not(unix))]
    let _ = target;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// A folder of its own for a test, removed afterwards.
    struct Folder(PathBuf);

    impl Folder {
        fn new(name: &str) -> Self {
            let folder = std::env::temp_dir()
                .join(format!("jot-file-write-test-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&folder);
            fs::create_dir_all(&folder).unwrap();
            Self(folder)
        }

        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }

        /// The names in the folder, sorted.
        fn names(&self) -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(&self.0)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Writes the first half of `data`, then fails, as a full disk would.
    fn fail_halfway(data: &[u8]) -> impl FnMut(&mut File) -> io::Result<()> + '_ {
        move |file| {
            file.write_all(&data[..data.len() / 2])?;
            Err(io::Error::other("the disk is full"))
        }
    }

    #[test]
    fn a_failed_save_leaves_the_file_as_it_was() {
        let folder = Folder::new("failed");
        let file = folder.path("notes.txt");
        fs::write(&file, "old text").unwrap();

        let error = write_whole_with(&file, &mut fail_halfway(b"new text, longer than the old"))
            .unwrap_err();
        assert_eq!(error.to_string(), "the disk is full");
        assert_eq!(fs::read_to_string(&file).unwrap(), "old text");
        assert_eq!(folder.names(), ["notes.txt"]);
    }

    #[test]
    fn a_failed_save_leaves_a_hard_linked_file_as_it_was() {
        let folder = Folder::new("failed-linked");
        let file = folder.path("notes.txt");
        let other = folder.path("other.txt");
        fs::write(&file, "old text").unwrap();
        fs::hard_link(&file, &other).unwrap();

        let error = write_whole_with(&file, &mut fail_halfway(b"new text, longer than the old"))
            .unwrap_err();
        assert_eq!(error.to_string(), "the disk is full");
        assert_eq!(fs::read_to_string(&file).unwrap(), "old text");
        assert_eq!(fs::read_to_string(&other).unwrap(), "old text");
        assert_eq!(folder.names(), ["notes.txt", "other.txt"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_save_through_a_symlink_writes_its_target_and_keeps_the_link() {
        use std::os::unix::fs::symlink;
        let folder = Folder::new("symlink");
        let link = folder.path("link.txt");
        fs::write(folder.path("real.txt"), "old").unwrap();
        symlink("real.txt", &link).unwrap();

        write_whole(&link, b"new").unwrap();
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_link(&link).unwrap(), Path::new("real.txt"));
        assert_eq!(fs::read_to_string(folder.path("real.txt")).unwrap(), "new");

        // A link to a file that doesn't exist yet makes the file.
        let dangling = folder.path("dangling.txt");
        symlink(folder.path("made.txt"), &dangling).unwrap();
        write_whole(&dangling, b"made").unwrap();
        assert!(
            fs::symlink_metadata(&dangling)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(folder.path("made.txt")).unwrap(), "made");
        assert_eq!(
            folder.names(),
            ["dangling.txt", "link.txt", "made.txt", "real.txt"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_save_keeps_the_files_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let folder = Folder::new("permissions");
        let file = folder.path("script.sh");
        for mode in [0o600, 0o640, 0o755] {
            fs::write(&file, "old").unwrap();
            fs::set_permissions(&file, fs::Permissions::from_mode(mode)).unwrap();
            write_whole(&file, b"new").unwrap();
            assert_eq!(fs::read_to_string(&file).unwrap(), "new");
            let saved = fs::metadata(&file).unwrap().permissions().mode() & 0o7777;
            assert_eq!(saved, mode, "{saved:o} isn't {mode:o}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_file_isnt_saved_over() {
        use std::os::unix::fs::PermissionsExt;
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } == 0 {
            // Root may write any file.
            return;
        }
        let folder = Folder::new("read-only");
        let file = folder.path("notes.txt");
        fs::write(&file, "old").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o444)).unwrap();

        let error = write_whole(&file, b"new").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(fs::read_to_string(&file).unwrap(), "old");
        assert_eq!(folder.names(), ["notes.txt"]);
    }

    #[test]
    fn a_hard_linked_file_keeps_its_links() {
        let folder = Folder::new("hard-link");
        let file = folder.path("notes.txt");
        let other = folder.path("other.txt");
        fs::write(&file, "old").unwrap();
        fs::hard_link(&file, &other).unwrap();

        write_whole(&file, b"new").unwrap();
        assert_eq!(fs::read_to_string(&other).unwrap(), "new");
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let inode = |path: &Path| fs::metadata(path).unwrap().ino();
            assert_eq!(inode(&file), inode(&other));
        }
        assert_eq!(folder.names(), ["notes.txt", "other.txt"]);
    }

    #[test]
    fn a_big_file_is_saved_whole() {
        // 24 MB that doesn't repeat, so a misplaced block shows.
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        let big: Vec<u8> = (0..24 << 20)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed as u8
            })
            .collect();
        for linked in [false, true] {
            let folder = Folder::new(if linked { "big-linked" } else { "big" });
            let file = folder.path("big.txt");
            fs::write(&file, "small").unwrap();
            if linked {
                fs::hard_link(&file, folder.path("other.txt")).unwrap();
            }
            write_whole(&file, &big).unwrap();
            assert!(fs::read(&file).unwrap() == big, "linked: {linked}");
            // And back to a small file, which ends where its text does.
            write_whole(&file, b"small again").unwrap();
            assert_eq!(fs::read(&file).unwrap(), b"small again");
            let expected: &[&str] = if linked {
                &["big.txt", "other.txt"]
            } else {
                &["big.txt"]
            };
            assert_eq!(folder.names(), expected);
        }
    }
}
