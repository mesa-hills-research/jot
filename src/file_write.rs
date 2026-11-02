//! Saving a file whole, so that a save that fails leaves the file as it was.
//!
//! A new file, and on Linux an existing one, is written to a temporary file
//! beside it, which then takes its name. An existing file is replaced only
//! when the new one can have its owner, group, permissions and extended
//! attributes, which hold its ACL and security label. Otherwise, and on
//! Windows and macOS, the file is written in place, after a copy of it is
//! made in a folder of the caller's, and the copy is put back if the write
//! fails. A save through a symbolic link writes the file the link leads to.
//!
//! # What a save promises
//!
//! - The path, through its links, ends up with the new contents whole. Or
//!   the save fails, and the file is as it was, with its contents, its
//!   other names (hard links), owner, group, permissions and, on Linux,
//!   extended attributes.
//! - Saves of one file by programs that use this module, jot in other
//!   processes included, take turns. Each locks the file from start to end,
//!   so their writes never mix and none puts old contents back over
//!   another's.
//! - When another program moves, replaces, relinks or changes the file's
//!   permissions during a save, the save notices before its change takes
//!   effect, puts back what it changed, and starts again on what the path
//!   leads to then. It checks before writing, after writing and just before
//!   the new file takes the name.
//! - After a crash, a file replaced whole has its old contents or its new.
//!   A file written in place may be cut short, with its old contents in the
//!   caller's folder. A new file never shows part written under its name.
//! - Cleanup removes only the temporary file and the copy the save made,
//!   once it has checked that the name still belongs to them.
//!
//! # What it doesn't
//!
//! - A program that doesn't lock the file, as most editors and scripts
//!   don't, can change it in the instant between the save's last check and
//!   its change, and lose that change or have it undone. No call on these
//!   systems compares and replaces in one step.
//! - Changes another program made before the save started are overwritten
//!   without a warning.
//! - On NFS, Linux keeps these locks per process, so two saves in one
//!   process don't wait for each other there, and other machines may not
//!   honor them.
//! - Extended attributes this user can't list, such as `trusted` ones, aren't
//!   compared.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};

/// How many times a save starts again when another program changes the file
/// or puts another in its place.
const ATTEMPTS: usize = 8;

/// Replaces the contents of the file at `path` with `data`, or makes the
/// file. The folder must exist. A file written in place is first copied
/// into `backups`, which is made if need be and should be the user's alone.
pub fn write_whole(path: &Path, data: &[u8], backups: &Path) -> io::Result<()> {
    write_whole_with(path, backups, &mut |_| Ok(true), &mut |file| {
        file.write_all(data)
    })
}

/// Replaces the contents of the file at `path` with what `update` makes of
/// them, or makes the file, as [`write_whole`] does. `update` gets them, or
/// `None` for a file that doesn't exist yet, while the save holds the file,
/// so that other updates come before or after it and never between its
/// read and its write. When it returns `None`, nothing is written. It may
/// run again when the save starts again.
// The recent files, which only Windows has, use it.
#[cfg_attr(not(any(target_os = "windows", test)), allow(dead_code))]
pub fn update_whole(
    path: &Path,
    backups: &Path,
    mut update: impl FnMut(Option<Vec<u8>>) -> Option<Vec<u8>>,
) -> io::Result<()> {
    let contents = std::cell::RefCell::new(None);
    write_whole_with(
        path,
        backups,
        &mut |file| {
            let old = match file {
                Some(file) => {
                    let mut old = Vec::new();
                    file.seek(SeekFrom::Start(0))?;
                    io::Read::read_to_end(file, &mut old)?;
                    Some(old)
                }
                None => None,
            };
            let new = update(old);
            let changed = new.is_some();
            *contents.borrow_mut() = new;
            Ok(changed)
        },
        &mut |file| file.write_all(contents.borrow().as_deref().unwrap_or_default()),
    )
}

/// [`write_whole`], with `write` writing the new contents to the file it's
/// given, each time the save starts. First `decide` says whether to write
/// at all, given the file as it is, which the save holds by then, or `None`
/// for one that doesn't exist.
fn write_whole_with(
    path: &Path,
    backups: &Path,
    decide: &mut dyn FnMut(Option<&mut File>) -> io::Result<bool>,
    write: &mut dyn FnMut(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    let mut attempts = 0;
    while attempts < ATTEMPTS {
        attempts += 1;
        let target = follow_links(path)?;
        let (mut file, readable) = match open_to_write(&target) {
            Ok(opened) => opened,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if !decide(None)? {
                    return Ok(());
                }
                match stage(path, &target, None, write)? {
                    Staged::Committed => return Ok(()),
                    _ => continue,
                }
            }
            Err(error) => return Err(error),
        };
        // Waits for a save of the same file, by this program or another, to
        // end. shortcut: Linux turns these locks into ones per process on
        // NFS, so there two saves in one process don't wait for each other.
        // Add a lock per file within the process if that comes up.
        let waited = match file.try_lock() {
            Ok(()) => false,
            Err(fs::TryLockError::WouldBlock) => {
                file.lock()?;
                true
            }
            Err(fs::TryLockError::Error(error)) => return Err(error),
        };
        let named = is_named_by(path, &target, &file)?;
        if named && !decide(Some(&mut file))? {
            return Ok(());
        }
        if named && save_locked(path, &target, &mut file, readable, backups, write)? {
            return Ok(());
        }
        // The save waited for may have put a new file in this one's place,
        // which is no reason to give up.
        if !named && waited {
            attempts -= 1;
        }
    }
    Err(io::Error::other(
        "Another program kept changing it or putting other files in its place",
    ))
}

/// `path`, or the file its symbolic links lead to.
fn follow_links(path: &Path) -> io::Result<PathBuf> {
    // As many as Linux follows.
    const MOST_LINKS: usize = 40;
    let mut target = path.to_path_buf();
    for links in 0..=MOST_LINKS {
        match fs::symlink_metadata(&target) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                if links == MOST_LINKS {
                    break;
                }
                let link = fs::read_link(&target)?;
                target = match target.parent() {
                    Some(folder) => folder.join(link),
                    None => link,
                };
            }
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => return Ok(target),
        }
    }
    Err(io::Error::other(format!(
        "{} leads through more than {MOST_LINKS} symbolic links",
        path.display()
    )))
}

/// Opens the file at `target` to write it, and to read it as well where
/// that's allowed, and says whether it may be read. Opening it to write
/// fails as writing it would, for a read-only file or one another program
/// holds, so that replacing it doesn't get around that.
fn open_to_write(target: &Path) -> io::Result<(File, bool)> {
    match OpenOptions::new().read(true).write(true).open(target) {
        Ok(file) => Ok((file, true)),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => OpenOptions::new()
            .write(true)
            .open(target)
            .map(|file| (file, false)),
        Err(error) => Err(error),
    }
}

/// Saves `file`, which `target` names and which this save has locked.
/// Returns `false`, having put back anything it changed, when a program put
/// another file in `target`'s place meanwhile.
fn save_locked(
    path: &Path,
    target: &Path,
    file: &mut File,
    readable: bool,
    backups: &Path,
    write: &mut dyn FnMut(&mut File) -> io::Result<()>,
) -> io::Result<bool> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt;
        // The file's other names would keep the old contents.
        if file.metadata()?.nlink() == 1 {
            match stage(path, target, Some(file), write)? {
                Staged::Committed => return Ok(true),
                Staged::StartAgain => return Ok(false),
                Staged::WriteInPlace => {}
            }
        }
    }
    if !readable {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "It can be written but not read, so it can\u{2019}t be copied before it\u{2019}s written",
        ));
    }
    write_in_place(path, target, file, backups, write)
}

/// How a staged save ended.
enum Staged {
    /// The file has the new contents.
    Committed,
    /// Nothing changed, and the save starts again on what the path leads to
    /// now.
    StartAgain,
    /// Nothing changed. The file is to be written in place, since a new file
    /// can't have all that decides who may read or change it.
    #[cfg(target_os = "linux")]
    WriteInPlace,
}

/// Writes the new contents to a temporary file beside `target`, which then
/// takes its name. With an `original`, which `target` names and this save
/// has locked, the new file takes its place. Without one, it takes a name
/// nothing has, and the save starts again if something takes it first.
fn stage(
    path: &Path,
    target: &Path,
    original: Option<&File>,
    write: &mut dyn FnMut(&mut File) -> io::Result<()>,
) -> io::Result<Staged> {
    let (staged_path, mut staged) = create_unique(
        |n| target.with_file_name(format!(".jot-save-{}-{n}.tmp", std::process::id())),
        OpenOptions::new().write(true).create_new(true),
    )?;
    let committed = (|| {
        #[cfg(target_os = "linux")]
        let before = original.map(access).transpose()?;
        #[cfg(target_os = "linux")]
        if let Some(original) = original
            && !can_match(original, &staged)?
        {
            return Ok(Staged::WriteInPlace);
        }
        write(&mut staged)?;
        staged.sync_all()?;
        match original {
            Some(original) => {
                // A program may have changed who may read or change it, or
                // given it another name, during the write.
                #[cfg(target_os = "linux")]
                if Some(access(original)?) != before || !same_extended_attributes(original, &staged)
                {
                    return Ok(Staged::StartAgain);
                }
                if !is_named_by(path, target, original)? {
                    return Ok(Staged::StartAgain);
                }
                fs::rename(&staged_path, target)?;
            }
            None => {
                // A link on the way may lead elsewhere by now.
                if follow_links(path)? != target {
                    return Ok(Staged::StartAgain);
                }
                match rename_to_new_name(&staged_path, target) {
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        return Ok(Staged::StartAgain);
                    }
                    renamed => renamed?,
                }
            }
        }
        sync_folder(target);
        // A program may have moved it, or pointed a link elsewhere, in the
        // last instant. The file stays where it went.
        if !is_named_by(path, target, &staged)? {
            return Ok(Staged::StartAgain);
        }
        Ok(Staged::Committed)
    })();
    // Gone once it has taken the name, unless that was by a hard link.
    remove_own(&staged_path, &staged);
    committed
}

/// Gives `staged` the owner, group and permissions of `original`, and
/// returns whether it then has all that decides who may read or change
/// `original`.
#[cfg(target_os = "linux")]
fn can_match(original: &File, staged: &File) -> io::Result<bool> {
    let metadata = original.metadata()?;
    if !take_owner(staged, &metadata) {
        return Ok(false);
    }
    // After the owner, since changing the owner can clear the setuid and
    // setgid bits.
    staged.set_permissions(metadata.permissions())?;
    // After the permissions, which change an ACL's mask.
    Ok(same_extended_attributes(original, staged))
}

/// What decides whether a new file may take a file's place, besides its
/// extended attributes.
#[cfg(target_os = "linux")]
#[derive(PartialEq)]
struct Access {
    /// Which file it is.
    identity: Identity,
    names: u64,
    mode: u32,
    owner: (u32, u32),
    /// When it last changed, which any change to the rest or to its
    /// extended attributes moves. Comparing the rest as well doesn't depend
    /// on how fine that time is.
    changed: (i64, i64),
}

#[cfg(target_os = "linux")]
fn access(file: &File) -> io::Result<Access> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    Ok(Access {
        identity: (metadata.dev(), metadata.ino()),
        names: metadata.nlink(),
        mode: metadata.mode(),
        owner: (metadata.uid(), metadata.gid()),
        changed: (metadata.ctime(), metadata.ctime_nsec()),
    })
}

/// Renames `from` to `to`, failing with `AlreadyExists`, having changed
/// nothing, when a file has that name.
#[cfg(target_os = "linux")]
fn rename_to_new_name(from: &Path, to: &Path) -> io::Result<()> {
    let (from_name, to_name) = (c_path(from)?, c_path(to)?);
    // SAFETY: both names end with a nul.
    let renamed = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from_name.as_ptr(),
            libc::AT_FDCWD,
            to_name.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if renamed == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        // A file system that can't rename without replacing.
        Some(libc::EINVAL | libc::ENOSYS) => fs::hard_link(from, to),
        _ => Err(error),
    }
}

#[cfg(target_os = "macos")]
fn rename_to_new_name(from: &Path, to: &Path) -> io::Result<()> {
    let (from_name, to_name) = (c_path(from)?, c_path(to)?);
    // SAFETY: both names end with a nul.
    let renamed =
        unsafe { libc::renamex_np(from_name.as_ptr(), to_name.as_ptr(), libc::RENAME_EXCL) };
    if renamed == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        // A file system that can't rename without replacing.
        Some(libc::ENOTSUP) => fs::hard_link(from, to),
        _ => Err(error),
    }
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn rename_to_new_name(from: &Path, to: &Path) -> io::Result<()> {
    fs::hard_link(from, to)
}

#[cfg(windows)]
fn rename_to_new_name(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};
    use windows::core::PCWSTR;
    let wide = |path: &Path| -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    };
    let (from, to) = (wide(from), wide(to));
    // SAFETY: both names end with a nul and outlive the call. Without
    // MOVEFILE_REPLACE_EXISTING the move fails when `to` exists.
    unsafe {
        MoveFileExW(
            PCWSTR(from.as_ptr()),
            PCWSTR(to.as_ptr()),
            MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(system_error)
}

/// `error` as the system error it is, so that its kind, such as
/// `AlreadyExists`, shows. The windows crate keeps it as an HRESULT, which
/// hides that.
#[cfg(windows)]
fn system_error(error: windows::core::Error) -> io::Error {
    let code = error.code().0 as u32;
    // An HRESULT that holds a Win32 error code.
    if code & 0xFFFF_0000 == 0x8007_0000 {
        io::Error::from_raw_os_error((code & 0xFFFF) as i32)
    } else {
        io::Error::other(error)
    }
}

/// `path` as the C library takes it.
#[cfg(unix)]
fn c_path(path: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt as _;
    Ok(std::ffi::CString::new(path.as_os_str().as_bytes())?)
}

/// Writes `file`, which `target` names, in place, after copying it into
/// `backups`. The copy is put back if the write fails or a program puts
/// another file in `target`'s place meanwhile, and is removed once the copy
/// is no longer needed. Returns `false` in the second case.
fn write_in_place(
    path: &Path,
    target: &Path,
    file: &mut File,
    backups: &Path,
    write: &mut dyn FnMut(&mut File) -> io::Result<()>,
) -> io::Result<bool> {
    let (backup_path, mut backup) = create_backup(backups, target)?;
    let copied = (|| {
        file.seek(SeekFrom::Start(0))?;
        io::copy(file, &mut backup)?;
        backup.sync_all()?;
        sync_folder(&backup_path);
        Ok(())
    })();
    if let Err(error) = copied {
        remove_own(&backup_path, &backup);
        return Err(error);
    }
    let written = overwrite(file, write).and_then(|()| is_named_by(path, target, file));
    if let Ok(true) = written {
        remove_own(&backup_path, &backup);
        return Ok(true);
    }
    let restored = backup
        .seek(SeekFrom::Start(0))
        .and_then(|_| overwrite(file, &mut |file| io::copy(&mut backup, file).map(drop)));
    match restored {
        Ok(()) => {
            remove_own(&backup_path, &backup);
            written
        }
        Err(_) => {
            let error = written.err().unwrap_or_else(|| {
                io::Error::other("Another program put a file in its place during the save")
            });
            Err(io::Error::new(
                error.kind(),
                format!(
                    "{error}. Its contents from before the save are in {}",
                    backup_path.display()
                ),
            ))
        }
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

/// A new, empty file in `backups`, which only this user may read, named
/// after `target` so that it can be told apart.
fn create_backup(backups: &Path, target: &Path) -> io::Result<(PathBuf, File)> {
    let mut folder = fs::DirBuilder::new();
    folder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut folder, 0o700);
    folder.create(backups)?;
    // The file's name, cut to 64 bytes where a character ends, so that the
    // whole name fits wherever the file's own did.
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    let mut end = name.len().min(64);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    let name = &name[..end];
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    create_unique(
        |n| backups.join(format!("{name}-{}-{n}.backup", std::process::id())),
        &options,
    )
}

/// Makes a new file with `options`, at the first of `path(0)`, `path(1)`
/// and so on that's free.
fn create_unique(
    path: impl Fn(u32) -> PathBuf,
    options: &OpenOptions,
) -> io::Result<(PathBuf, File)> {
    let mut n = 0;
    loop {
        let path = path(n);
        match options.open(&path) {
            // One left by an earlier program with the same process id, or
            // made by another save of this one's.
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists && n < 1000 => n += 1,
            opened => return opened.map(|file| (path, file)),
        }
    }
}

/// Removes the name `path` of `file`, which this save made and no longer
/// needs, if the name still belongs to it. One that stays is noted in the
/// log, and its name says what made it.
fn remove_own(path: &Path, file: &File) {
    let removed = file_identity(file).and_then(|file| {
        if path_identity(path, false)? == Some(file) {
            fs::remove_file(path)
        } else {
            Ok(())
        }
    });
    if let Err(error) = removed
        && error.kind() != io::ErrorKind::NotFound
    {
        log::warn!("Couldn\u{2019}t remove {}. {error}", path.display());
    }
}

/// Whether `target` is `file`, and `path`, through its links, leads to it,
/// so that no program put another file in their place since `file` was
/// opened.
fn is_named_by(path: &Path, target: &Path, file: &File) -> io::Result<bool> {
    let file = Some(file_identity(file)?);
    Ok(path_identity(target, false)? == file && path_identity(path, true)? == file)
}

/// What tells a file apart from every other on the system.
#[cfg(unix)]
type Identity = (u64, u64);

#[cfg(unix)]
fn file_identity(file: &File) -> io::Result<Identity> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}

/// The identity of the file at `path`, or of the link there unless
/// `follow`, and `None` when nothing is there.
#[cfg(unix)]
fn path_identity(path: &Path, follow: bool) -> io::Result<Option<Identity>> {
    use std::os::unix::fs::MetadataExt;
    let metadata = if follow {
        fs::metadata(path)
    } else {
        fs::symlink_metadata(path)
    };
    match metadata {
        Ok(metadata) => Ok(Some((metadata.dev(), metadata.ino()))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// What tells a file apart from every other on the system: its volume and
/// its index there.
#[cfg(windows)]
type Identity = (u32, u64);

#[cfg(windows)]
fn file_identity(file: &File) -> io::Result<Identity> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the handle is the open file's, and `info` outlives the call.
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }
        .map_err(system_error)?;
    let index = u64::from(info.nFileIndexHigh) << 32 | u64::from(info.nFileIndexLow);
    Ok((info.dwVolumeSerialNumber, index))
}

/// The identity of the file at `path`, or of the link there unless
/// `follow`, and `None` when nothing is there.
#[cfg(windows)]
fn path_identity(path: &Path, follow: bool) -> io::Result<Option<Identity>> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
    let mut options = OpenOptions::new();
    // No access but to what the handle tells, so that a file another
    // program holds opens too.
    options.access_mode(0);
    if !follow {
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0);
    }
    match options.open(path) {
        Ok(file) => file_identity(&file).map(Some),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Gives `file` the owner and group of `original`, and returns whether it
/// has them. Only the superuser may give a file away, while anyone may give
/// one of their own files to a group they belong to.
#[cfg(target_os = "linux")]
fn take_owner(file: &File, original: &fs::Metadata) -> bool {
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

/// Whether `a` and `b` have the same extended attributes, which on Linux
/// hold a file's ACL and security label. Those this user may not list,
/// such as `trusted` ones, aren't compared.
#[cfg(target_os = "linux")]
fn same_extended_attributes(a: &File, b: &File) -> bool {
    matches!(
        (extended_attributes(a), extended_attributes(b)),
        (Ok(a), Ok(b)) if a == b
    )
}

/// The names and values of `file`'s extended attributes, by name.
#[cfg(target_os = "linux")]
fn extended_attributes(file: &File) -> io::Result<Vec<(Vec<u8>, Vec<u8>)>> {
    use std::os::fd::AsRawFd;
    let fd = file.as_raw_fd();
    // SAFETY: the buffer is as long as the length given with it.
    let names = read_sized(|buffer, length| unsafe { libc::flistxattr(fd, buffer.cast(), length) });
    let names = match names {
        // A file system that keeps none.
        Err(error) if error.raw_os_error() == Some(libc::ENOTSUP) => Vec::new(),
        names => names?,
    };
    let mut attributes = Vec::new();
    for name in names
        .split(|&byte| byte == 0)
        .filter(|name| !name.is_empty())
    {
        let name = std::ffi::CString::new(name)?;
        // SAFETY: as above, and `name` ends with a nul.
        let value = read_sized(|buffer, length| unsafe {
            libc::fgetxattr(fd, name.as_ptr(), buffer.cast(), length)
        })?;
        attributes.push((name.into_bytes(), value));
    }
    attributes.sort();
    Ok(attributes)
}

/// What `read` reads into a buffer of the length it says it needs when
/// given none, asking again when that grew in between.
#[cfg(target_os = "linux")]
fn read_sized(read: impl Fn(*mut u8, usize) -> isize) -> io::Result<Vec<u8>> {
    loop {
        let needed = usize::try_from(read(std::ptr::null_mut(), 0))
            .map_err(|_| io::Error::last_os_error())?;
        let mut buffer = vec![0; needed];
        match usize::try_from(read(buffer.as_mut_ptr(), buffer.len())) {
            Ok(length) => {
                buffer.truncate(length);
                return Ok(buffer);
            }
            Err(_) => {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ERANGE) {
                    return Err(error);
                }
            }
        }
    }
}

/// Makes the name of a file in `path`'s folder, just made or renamed, last
/// through a power cut. The file's contents are synced by then, and some
/// file systems can't sync a folder, so a failure is ignored. Windows syncs
/// no folders.
fn sync_folder(path: &Path) {
    #[cfg(unix)]
    {
        let folder = match path.parent() {
            Some(folder) if !folder.as_os_str().is_empty() => folder,
            _ => Path::new("."),
        };
        if let Ok(folder) = File::open(folder) {
            let _ = folder.sync_all();
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Read as _;
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    /// Folders of its own for a test, removed afterwards. One holds the
    /// files it saves, and one the copies kept while they're written in
    /// place.
    struct Folder(PathBuf);

    impl Folder {
        fn new(name: &str) -> Self {
            let folder = std::env::temp_dir()
                .join(format!("jot-file-write-test-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&folder);
            fs::create_dir_all(folder.join("files")).unwrap();
            Self(folder)
        }

        fn files(&self) -> PathBuf {
            self.0.join("files")
        }

        fn path(&self, name: &str) -> PathBuf {
            self.files().join(name)
        }

        fn backups(&self) -> PathBuf {
            self.0.join("backups")
        }

        /// The names in the files' folder, sorted.
        fn names(&self) -> Vec<String> {
            names_in(&self.files())
        }

        /// The copies left in the backups folder, sorted.
        fn backups_left(&self) -> Vec<String> {
            names_in(&self.backups())
        }

        fn save(&self, path: &Path, data: &[u8]) -> io::Result<()> {
            write_whole(path, data, &self.backups())
        }

        fn save_with(
            &self,
            path: &Path,
            write: &mut dyn FnMut(&mut File) -> io::Result<()>,
        ) -> io::Result<()> {
            write_whole_with(path, &self.backups(), &mut |_| Ok(true), write)
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// The names in `folder`, sorted. None for a folder that isn't there.
    fn names_in(folder: &Path) -> Vec<String> {
        let Ok(entries) = fs::read_dir(folder) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// Writes the first half of `data`, then fails, as a full disk would.
    fn fail_halfway(data: &[u8]) -> impl FnMut(&mut File) -> io::Result<()> + '_ {
        move |file| {
            file.write_all(&data[..data.len() / 2])?;
            Err(io::Error::other("the disk is full"))
        }
    }

    /// Whether a save holds the file at `path`, so that another handle can't
    /// lock it.
    fn is_held(path: &Path) -> bool {
        let file = File::open(path).unwrap();
        matches!(file.try_lock(), Err(fs::TryLockError::WouldBlock))
    }

    #[cfg(unix)]
    fn is_root() -> bool {
        // SAFETY: geteuid has no preconditions.
        unsafe { libc::geteuid() == 0 }
    }

    #[test]
    fn a_failed_save_leaves_the_file_as_it_was() {
        let folder = Folder::new("failed");
        let file = folder.path("notes.txt");
        fs::write(&file, "old text").unwrap();

        let error = folder
            .save_with(&file, &mut fail_halfway(b"new text, longer than the old"))
            .unwrap_err();
        assert_eq!(error.to_string(), "the disk is full");
        assert_eq!(fs::read_to_string(&file).unwrap(), "old text");
        assert_eq!(folder.names(), ["notes.txt"]);
        assert!(folder.backups_left().is_empty());
    }

    #[test]
    fn a_failed_save_leaves_a_hard_linked_file_as_it_was() {
        let folder = Folder::new("failed-linked");
        let file = folder.path("notes.txt");
        let other = folder.path("other.txt");
        fs::write(&file, "old text").unwrap();
        fs::hard_link(&file, &other).unwrap();

        let error = folder
            .save_with(&file, &mut fail_halfway(b"new text, longer than the old"))
            .unwrap_err();
        assert_eq!(error.to_string(), "the disk is full");
        assert_eq!(fs::read_to_string(&file).unwrap(), "old text");
        assert_eq!(fs::read_to_string(&other).unwrap(), "old text");
        assert_eq!(folder.names(), ["notes.txt", "other.txt"]);
        assert!(folder.backups_left().is_empty());
    }

    #[test]
    fn a_save_that_keeps_failing_keeps_the_copy_and_says_where() {
        let folder = Folder::new("keeps-failing");
        let file = folder.path("notes.txt");
        fs::write(&file, "old text").unwrap();
        // A file with another name is written in place on every system.
        fs::hard_link(&file, folder.path("other.txt")).unwrap();

        let error = folder
            .save_with(&file, &mut |written| {
                written.write_all(b"new")?;
                // Every write fails from here on, putting the old text back
                // too, as on a disk that stays full.
                *written = File::open(&file)?;
                Err(io::Error::other("The disk is full"))
            })
            .unwrap_err();
        let kept = folder.backups_left();
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert!(kept[0].starts_with("notes.txt"), "{kept:?}");
        let backup = folder.backups().join(&kept[0]);
        assert_eq!(fs::read_to_string(&backup).unwrap(), "old text");
        assert!(
            error.to_string().contains(&backup.display().to_string()),
            "{error}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&backup).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{mode:o}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_save_through_a_symlink_writes_its_target_and_keeps_the_link() {
        use std::os::unix::fs::symlink;
        let folder = Folder::new("symlink");
        let link = folder.path("link.txt");
        fs::write(folder.path("real.txt"), "old").unwrap();
        symlink("real.txt", &link).unwrap();

        folder.save(&link, b"new").unwrap();
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
        folder.save(&dangling, b"made").unwrap();
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

    #[cfg(target_os = "linux")]
    #[test]
    fn forty_links_lead_to_the_file_and_a_forty_first_is_too_many() {
        use std::os::unix::fs::symlink;
        let folder = Folder::new("forty-links");
        fs::write(folder.path("real.txt"), "old").unwrap();
        symlink("real.txt", folder.path("link-1")).unwrap();
        for n in 2..=41 {
            symlink(format!("link-{}", n - 1), folder.path(&format!("link-{n}"))).unwrap();
        }

        folder.save(&folder.path("link-40"), b"new").unwrap();
        assert_eq!(fs::read_to_string(folder.path("real.txt")).unwrap(), "new");
        assert!(folder.save(&folder.path("link-41"), b"newer").is_err());
        assert_eq!(fs::read_to_string(folder.path("real.txt")).unwrap(), "new");
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
            folder.save(&file, b"new").unwrap();
            assert_eq!(fs::read_to_string(&file).unwrap(), "new");
            let saved = fs::metadata(&file).unwrap().permissions().mode() & 0o7777;
            assert_eq!(saved, mode, "{saved:o} isn't {mode:o}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_file_isnt_saved_over() {
        use std::os::unix::fs::PermissionsExt;
        if is_root() {
            // Root may write any file.
            return;
        }
        let folder = Folder::new("read-only");
        let file = folder.path("notes.txt");
        fs::write(&file, "old").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o444)).unwrap();

        let error = folder.save(&file, b"new").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(fs::read_to_string(&file).unwrap(), "old");
        assert_eq!(folder.names(), ["notes.txt"]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_write_only_file_is_saved_unless_it_must_be_copied_first() {
        use std::os::unix::fs::PermissionsExt;
        if is_root() {
            // Root may read any file.
            return;
        }
        let folder = Folder::new("write-only");
        let file = folder.path("notes.txt");
        let mode = |mode| fs::Permissions::from_mode(mode);
        fs::write(&file, "old").unwrap();
        fs::set_permissions(&file, mode(0o200)).unwrap();

        folder.save(&file, b"new").unwrap();
        let saved = fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(saved, 0o200, "{saved:o}");
        fs::set_permissions(&file, mode(0o600)).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "new");

        // A file with another name is written in place, after a copy of it
        // is made, which a file that can't be read can't have.
        fs::hard_link(&file, folder.path("other.txt")).unwrap();
        fs::set_permissions(&file, mode(0o200)).unwrap();
        let error = folder.save(&file, b"newer").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        fs::set_permissions(&file, mode(0o600)).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "new");
        assert_eq!(folder.names(), ["notes.txt", "other.txt"]);
        assert!(folder.backups_left().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_file_with_a_long_name_is_saved() {
        // 245 bytes, under the 255 most file systems allow.
        let name = format!("{}.txt", "n".repeat(241));
        for linked in [false, true] {
            let folder = Folder::new(if linked { "long-linked" } else { "long" });
            let file = folder.path(&name);
            fs::write(&file, "old").unwrap();
            if linked {
                fs::hard_link(&file, folder.path("other.txt")).unwrap();
            }
            folder.save(&file, b"new").unwrap();
            assert_eq!(
                fs::read_to_string(&file).unwrap(),
                "new",
                "linked: {linked}"
            );
            assert!(folder.backups_left().is_empty());
        }
    }

    #[test]
    fn a_hard_linked_file_keeps_its_links() {
        let folder = Folder::new("hard-link");
        let file = folder.path("notes.txt");
        let other = folder.path("other.txt");
        fs::write(&file, "old").unwrap();
        fs::hard_link(&file, &other).unwrap();

        folder.save(&file, b"new").unwrap();
        assert_eq!(fs::read_to_string(&other).unwrap(), "new");
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let inode = |path: &Path| fs::metadata(path).unwrap().ino();
            assert_eq!(inode(&file), inode(&other));
        }
        assert_eq!(folder.names(), ["notes.txt", "other.txt"]);
        assert!(folder.backups_left().is_empty());
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
            folder.save(&file, &big).unwrap();
            assert!(fs::read(&file).unwrap() == big, "linked: {linked}");
            // And back to a small file, which ends where its text does.
            folder.save(&file, b"small again").unwrap();
            assert_eq!(fs::read(&file).unwrap(), b"small again");
            let expected: &[&str] = if linked {
                &["big.txt", "other.txt"]
            } else {
                &["big.txt"]
            };
            assert_eq!(folder.names(), expected);
            assert!(folder.backups_left().is_empty());
        }
    }

    #[test]
    fn a_save_holds_the_file_until_it_ends() {
        for linked in [false, true] {
            let folder = Folder::new(if linked { "held-linked" } else { "held" });
            let file = folder.path("notes.txt");
            fs::write(&file, "old").unwrap();
            if linked {
                fs::hard_link(&file, folder.path("other.txt")).unwrap();
            }

            let mut held = false;
            folder
                .save_with(&file, &mut |written| {
                    held = is_held(&file);
                    written.write_all(b"new")
                })
                .unwrap();
            assert!(held, "linked: {linked}");
            assert!(!is_held(&file), "linked: {linked}");

            // A save that fails lets go of it too.
            folder
                .save_with(&file, &mut fail_halfway(b"newer"))
                .unwrap_err();
            assert!(!is_held(&file), "linked: {linked}");
            assert_eq!(fs::read_to_string(&file).unwrap(), "new");
        }
    }

    #[test]
    fn saves_at_once_each_leave_the_file_whole() {
        const SAVES: usize = 40;
        for linked in [false, true] {
            let folder = Folder::new(if linked { "at-once-linked" } else { "at-once" });
            let file = folder.path("notes.txt");
            fs::write(&file, "old").unwrap();
            if linked {
                fs::hard_link(&file, folder.path("other.txt")).unwrap();
            }
            // Of different lengths, so that a mix of two shows.
            let texts: Vec<Vec<u8>> = (0..4u8)
                .map(|n| vec![b'a' + n; 3000 * (usize::from(n) + 1)])
                .collect();

            let (progress, made) = mpsc::channel();
            for text in texts.clone() {
                let (progress, file, backups, texts) = (
                    progress.clone(),
                    file.clone(),
                    folder.backups(),
                    texts.clone(),
                );
                thread::spawn(move || {
                    for _ in 0..SAVES {
                        let saved =
                            write_whole_with(&file, &backups, &mut |_| Ok(true), &mut |written| {
                                for piece in text.chunks(500) {
                                    written.write_all(piece)?;
                                    thread::yield_now();
                                }
                                Ok(())
                            });
                        // A reader that takes the lock finds one whole text.
                        let read = saved.and_then(|()| {
                            let mut reader = File::open(&file)?;
                            reader.lock_shared()?;
                            let mut read = Vec::new();
                            reader.read_to_end(&mut read)?;
                            Ok(read)
                        });
                        let whole = read
                            .map(|read| texts.contains(&read))
                            .map_err(|error| error.to_string());
                        if progress.send(whole).is_err() {
                            return;
                        }
                    }
                });
            }
            drop(progress);
            for _ in 0..texts.len() * SAVES {
                let whole = made
                    .recv_timeout(Duration::from_secs(30))
                    .expect("the saves stopped making progress");
                assert_eq!(whole, Ok(true), "linked: {linked}");
            }
            assert!(folder.backups_left().is_empty());
        }
    }

    #[test]
    fn updates_at_once_each_see_the_ones_before() {
        const UPDATES: usize = 25;
        let folder = Folder::new("updates-at-once");
        let file = folder.path("count.txt");
        let (progress, made) = mpsc::channel();
        for _ in 0..4 {
            let (progress, file, backups) = (progress.clone(), file.clone(), folder.backups());
            thread::spawn(move || {
                for _ in 0..UPDATES {
                    let updated = update_whole(&file, &backups, |old| {
                        let count: usize =
                            old.map_or(0, |old| String::from_utf8_lossy(&old).parse().unwrap());
                        thread::yield_now();
                        Some((count + 1).to_string().into_bytes())
                    });
                    if progress
                        .send(updated.map_err(|error| error.to_string()))
                        .is_err()
                    {
                        return;
                    }
                }
            });
        }
        drop(progress);
        for _ in 0..4 * UPDATES {
            let updated = made
                .recv_timeout(Duration::from_secs(30))
                .expect("the updates stopped making progress");
            assert_eq!(updated, Ok(()));
        }
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            (4 * UPDATES).to_string()
        );

        // An update that changes nothing leaves the file alone.
        let before = fs::metadata(&file).unwrap().modified().unwrap();
        update_whole(&file, &folder.backups(), |old| {
            assert_eq!(old.as_deref(), Some(&b"100"[..]));
            None
        })
        .unwrap();
        assert_eq!(fs::metadata(&file).unwrap().modified().unwrap(), before);
        assert_eq!(folder.names(), ["count.txt"]);
        assert!(folder.backups_left().is_empty());
    }

    /// Saves `notes.txt` in a folder of its own, while `meanwhile`, given
    /// the file's path, puts another file in its place partway through.
    /// Returns the folder and what the save returned.
    fn save_while(
        name: &str,
        linked: bool,
        mut meanwhile: impl FnMut(&Path) -> io::Result<()>,
    ) -> (Folder, io::Result<()>) {
        let folder = Folder::new(&format!("{name}-{linked}"));
        let file = folder.path("notes.txt");
        fs::write(&file, "old").unwrap();
        if linked {
            fs::hard_link(&file, folder.path("other.txt")).unwrap();
        }
        let mut first = true;
        let saved = folder.save_with(&file, &mut |written| {
            if std::mem::take(&mut first) {
                // Another program moves the file away first.
                fs::rename(&file, folder.path("moved.txt"))?;
                meanwhile(&file)?;
            }
            written.write_all(b"new")
        });
        (folder, saved)
    }

    #[test]
    fn a_file_put_in_its_place_during_a_save_gets_the_text() {
        for linked in [false, true] {
            let (folder, saved) = save_while("replaced", linked, |file| {
                fs::write(file, "another program's")
            });
            saved.unwrap();
            let read = |name| fs::read_to_string(folder.path(name)).unwrap();
            assert_eq!(read("notes.txt"), "new", "linked: {linked}");
            assert_eq!(read("moved.txt"), "old", "linked: {linked}");
            assert!(folder.backups_left().is_empty());
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_file_put_in_its_place_during_a_save_is_left_alone() {
        use std::os::unix::fs::PermissionsExt;
        if is_root() {
            return;
        }
        for linked in [false, true] {
            let (folder, saved) = save_while("replaced-read-only", linked, |file| {
                fs::write(file, "another program's")?;
                fs::set_permissions(file, fs::Permissions::from_mode(0o444))
            });
            assert_eq!(
                saved.unwrap_err().kind(),
                io::ErrorKind::PermissionDenied,
                "linked: {linked}"
            );
            let file = folder.path("notes.txt");
            assert_eq!(fs::read_to_string(&file).unwrap(), "another program's");
            let mode = fs::metadata(&file).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o444, "{mode:o}");
            assert_eq!(fs::read_to_string(folder.path("moved.txt")).unwrap(), "old");
            assert!(folder.backups_left().is_empty());
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_link_put_in_its_place_during_a_save_is_followed() {
        use std::os::unix::fs::symlink;
        for linked in [false, true] {
            let (folder, saved) = save_while("replaced-by-link", linked, |file| {
                fs::write(file.with_file_name("elsewhere.txt"), "elsewhere")?;
                symlink("elsewhere.txt", file)
            });
            saved.unwrap();
            let file = folder.path("notes.txt");
            assert!(fs::symlink_metadata(&file).unwrap().is_symlink());
            let read = |name| fs::read_to_string(folder.path(name)).unwrap();
            assert_eq!(read("elsewhere.txt"), "new", "linked: {linked}");
            assert_eq!(read("moved.txt"), "old", "linked: {linked}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_link_pointed_elsewhere_during_a_save_has_its_new_target_written() {
        use std::os::unix::fs::symlink;
        for linked in [false, true] {
            let folder = Folder::new(&format!("retargeted-{linked}"));
            let link = folder.path("link.txt");
            fs::write(folder.path("first.txt"), "first").unwrap();
            fs::write(folder.path("second.txt"), "second").unwrap();
            if linked {
                fs::hard_link(folder.path("first.txt"), folder.path("other.txt")).unwrap();
            }
            symlink("first.txt", &link).unwrap();
            let mut first = true;
            folder
                .save_with(&link, &mut |written| {
                    if std::mem::take(&mut first) {
                        symlink("second.txt", folder.path("new-link.txt"))?;
                        fs::rename(folder.path("new-link.txt"), &link)?;
                    }
                    written.write_all(b"new")
                })
                .unwrap();
            let read = |name| fs::read_to_string(folder.path(name)).unwrap();
            assert_eq!(read("link.txt"), "new", "linked: {linked}");
            assert_eq!(read("second.txt"), "new", "linked: {linked}");
            assert_eq!(read("first.txt"), "first", "linked: {linked}");
        }
    }

    #[test]
    fn leftovers_of_an_earlier_save_dont_get_in_the_way() {
        let folder = Folder::new("leftovers");
        let file = folder.path("notes.txt");
        fs::write(&file, "old").unwrap();
        fs::hard_link(&file, folder.path("other.txt")).unwrap();
        // What a save stopped by a crash leaves, by a jot that had this
        // process id.
        let pid = std::process::id();
        let temporary = format!(".jot-save-{pid}-0.tmp");
        let backup = format!("notes.txt-{pid}-0.backup");
        fs::create_dir_all(folder.backups()).unwrap();
        fs::write(folder.path(&temporary), "left behind").unwrap();
        fs::write(folder.backups().join(&backup), "left behind").unwrap();

        folder.save(&file, b"new").unwrap();
        fs::remove_file(folder.path("other.txt")).unwrap();
        folder.save(&file, b"newer").unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "newer");
        assert_eq!(folder.names(), [temporary.clone(), "notes.txt".into()]);
        assert_eq!(folder.backups_left(), std::slice::from_ref(&backup));
        let read = |path: PathBuf| fs::read_to_string(path).unwrap();
        assert_eq!(read(folder.path(&temporary)), "left behind");
        assert_eq!(read(folder.backups().join(&backup)), "left behind");
    }

    #[test]
    fn a_file_put_at_a_new_files_name_during_a_save_gets_the_text() {
        let folder = Folder::new("new-replaced");
        let file = folder.path("notes.txt");
        let mut first = true;
        folder
            .save_with(&file, &mut |written| {
                if std::mem::take(&mut first) {
                    // Whatever has the name moves away, and another
                    // program puts a file of its own there.
                    if file.exists() {
                        fs::rename(&file, folder.path("moved.txt"))?;
                    }
                    fs::write(&file, "another program's")?;
                }
                written.write_all(b"new")
            })
            .unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "new");
        assert_eq!(folder.names(), ["notes.txt"]);
    }

    #[test]
    fn a_failed_save_of_a_new_file_leaves_a_file_put_at_its_name() {
        let folder = Folder::new("new-replaced-failed");
        let file = folder.path("notes.txt");
        let error = folder
            .save_with(&file, &mut |written| {
                written.write_all(b"part")?;
                if file.exists() {
                    fs::rename(&file, folder.path("moved.txt"))?;
                }
                fs::write(&file, "another program's")?;
                Err(io::Error::other("The disk is full"))
            })
            .unwrap_err();
        assert_eq!(error.to_string(), "The disk is full");
        assert_eq!(fs::read_to_string(&file).unwrap(), "another program's");
        assert_eq!(folder.names(), ["notes.txt"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_to_a_new_file_pointed_elsewhere_during_a_save_has_its_new_target_written() {
        use std::os::unix::fs::symlink;
        let folder = Folder::new("new-retargeted");
        let link = folder.path("link.txt");
        symlink("first.txt", &link).unwrap();
        fs::write(folder.path("second.txt"), "second").unwrap();
        let mut first = true;
        folder
            .save_with(&link, &mut |written| {
                if std::mem::take(&mut first) {
                    symlink("second.txt", folder.path("new-link.txt"))?;
                    fs::rename(folder.path("new-link.txt"), &link)?;
                }
                written.write_all(b"new")
            })
            .unwrap();
        assert_eq!(fs::read_to_string(&link).unwrap(), "new");
        assert_eq!(folder.names(), ["link.txt", "second.txt"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_file_with_a_long_name_in_other_scripts_is_saved() {
        // 244 bytes, of 60 characters of four bytes each.
        let names = [format!("{}.txt", "\u{1f4dd}".repeat(60))];
        #[cfg(target_os = "linux")]
        let names = {
            use std::os::unix::ffi::OsStringExt;
            // 250 bytes that aren't UTF-8, which Linux allows in a name.
            let other = std::ffi::OsString::from_vec(vec![0xff; 250]);
            [names[0].clone().into(), other]
        };
        for name in names {
            let folder = Folder::new("long-scripts");
            let file = folder.files().join(&name);
            fs::write(&file, "old").unwrap();
            // Written in place, after a copy of it named after it.
            fs::hard_link(&file, folder.path("other.txt")).unwrap();
            folder.save(&file, b"new").unwrap();
            assert_eq!(fs::read_to_string(&file).unwrap(), "new");
            assert!(folder.backups_left().is_empty());
        }
    }

    #[cfg(target_os = "linux")]
    mod linux {
        use super::*;

        const USER_OBJ: u16 = 0x01;
        const USER: u16 = 0x02;
        const GROUP_OBJ: u16 = 0x04;
        const MASK: u16 = 0x10;
        const OTHER: u16 = 0x20;
        /// The id of an entry that names no one in particular.
        const ANYONE: u32 = u32::MAX;
        /// The user nobody.
        const NOBODY: u32 = 65534;

        /// A POSIX ACL as Linux keeps it in an extended attribute, from its
        /// entries' tags, permissions and user or group ids.
        fn acl(entries: &[(u16, u16, u32)]) -> Vec<u8> {
            let mut acl = 2u32.to_le_bytes().to_vec();
            for (tag, permissions, id) in entries {
                acl.extend(tag.to_le_bytes());
                acl.extend(permissions.to_le_bytes());
                acl.extend(id.to_le_bytes());
            }
            acl
        }

        fn set_attribute(path: &Path, name: &str, value: &[u8]) -> io::Result<()> {
            use std::os::unix::ffi::OsStrExt;
            let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
            let name = std::ffi::CString::new(name).unwrap();
            // SAFETY: both strings end with a nul, and `value` is as long as
            // the length given with it.
            let set = unsafe {
                libc::setxattr(
                    path.as_ptr(),
                    name.as_ptr(),
                    value.as_ptr().cast(),
                    value.len(),
                    0,
                )
            };
            if set == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }

        fn attributes(path: &Path) -> Vec<(Vec<u8>, Vec<u8>)> {
            extended_attributes(&File::open(path).unwrap()).unwrap()
        }

        fn inode(path: &Path) -> u64 {
            use std::os::unix::fs::MetadataExt;
            fs::metadata(path).unwrap().ino()
        }

        #[test]
        fn a_save_keeps_the_files_acl_and_extended_attributes() {
            let folder = Folder::new("attributes");
            let file = folder.path("notes.txt");
            fs::write(&file, "old").unwrap();
            // Nobody may read it, whoever else may.
            let denial = acl(&[
                (USER_OBJ, 6, ANYONE),
                (USER, 0, NOBODY),
                (GROUP_OBJ, 4, ANYONE),
                (MASK, 4, ANYONE),
                (OTHER, 4, ANYONE),
            ]);
            if set_attribute(&file, "system.posix_acl_access", &denial)
                .and_then(|()| set_attribute(&file, "user.jot-test", b"kept"))
                .is_err()
            {
                eprintln!("Skipped, since the file system keeps no ACLs or extended attributes");
                return;
            }
            let before = attributes(&file);
            let names: Vec<&[u8]> = before.iter().map(|(name, _)| &name[..]).collect();
            assert!(
                names.contains(&&b"system.posix_acl_access"[..]),
                "{before:?}"
            );

            folder.save(&file, b"new").unwrap();
            assert_eq!(fs::read_to_string(&file).unwrap(), "new");
            assert_eq!(attributes(&file), before);
            assert_eq!(folder.names(), ["notes.txt"]);
            assert!(folder.backups_left().is_empty());
        }

        #[test]
        fn a_save_takes_no_acl_from_the_folder() {
            let folder = Folder::new("folder-acl");
            let file = folder.path("notes.txt");
            fs::write(&file, "old").unwrap();
            let before = attributes(&file);
            // New files in the folder would let nobody read and write them.
            let inherited = acl(&[
                (USER_OBJ, 7, ANYONE),
                (USER, 6, NOBODY),
                (GROUP_OBJ, 5, ANYONE),
                (MASK, 7, ANYONE),
                (OTHER, 5, ANYONE),
            ]);
            if set_attribute(&folder.files(), "system.posix_acl_default", &inherited).is_err() {
                eprintln!("Skipped, since the file system keeps no ACLs");
                return;
            }

            folder.save(&file, b"new").unwrap();
            assert_eq!(fs::read_to_string(&file).unwrap(), "new");
            assert_eq!(attributes(&file), before);
            assert_eq!(folder.names(), ["notes.txt"]);
        }

        /// Saves `notes.txt`, plain and with mode 0644, while `meanwhile`
        /// changes it partway through, once.
        fn save_while_changed(
            name: &str,
            mut meanwhile: impl FnMut(&Path) -> io::Result<()>,
        ) -> Folder {
            use std::os::unix::fs::PermissionsExt;
            let folder = Folder::new(name);
            let file = folder.path("notes.txt");
            fs::write(&file, "old").unwrap();
            fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
            let mut first = true;
            folder
                .save_with(&file, &mut |written| {
                    if std::mem::take(&mut first) {
                        meanwhile(&file)?;
                    }
                    written.write_all(b"new")
                })
                .unwrap();
            assert_eq!(fs::read_to_string(&file).unwrap(), "new");
            folder
        }

        #[test]
        fn permissions_narrowed_during_a_save_stay_narrowed() {
            use std::os::unix::fs::PermissionsExt;
            let folder = save_while_changed("narrowed", |file| {
                fs::set_permissions(file, fs::Permissions::from_mode(0o600))
            });
            let mode = fs::metadata(folder.path("notes.txt"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "{mode:o}");
            assert_eq!(folder.names(), ["notes.txt"]);
        }

        #[test]
        fn a_name_given_to_the_file_during_a_save_keeps_it() {
            let folder = save_while_changed("linked-meanwhile", |file| {
                fs::hard_link(file, file.with_file_name("other.txt"))
            });
            let read = |name| fs::read_to_string(folder.path(name)).unwrap();
            assert_eq!(read("other.txt"), "new");
            assert_eq!(
                inode(&folder.path("other.txt")),
                inode(&folder.path("notes.txt"))
            );
        }

        #[test]
        fn an_acl_given_to_the_file_during_a_save_is_kept() {
            let denial = acl(&[
                (USER_OBJ, 6, ANYONE),
                (USER, 0, NOBODY),
                (GROUP_OBJ, 4, ANYONE),
                (MASK, 4, ANYONE),
                (OTHER, 4, ANYONE),
            ]);
            let probe = Folder::new("acl-support");
            fs::write(probe.path("probe"), "").unwrap();
            if set_attribute(&probe.path("probe"), "system.posix_acl_access", &denial).is_err() {
                eprintln!("Skipped, since the file system keeps no ACLs");
                return;
            }
            let folder = save_while_changed("acl-meanwhile", |file| {
                set_attribute(file, "system.posix_acl_access", &denial)
            });
            let names: Vec<Vec<u8>> = attributes(&folder.path("notes.txt"))
                .into_iter()
                .map(|(name, _)| name)
                .collect();
            assert_eq!(names, [b"system.posix_acl_access".to_vec()]);
        }

        #[test]
        fn a_plain_file_is_replaced_by_a_new_one() {
            let folder = Folder::new("replaced-whole");
            let file = folder.path("notes.txt");
            fs::write(&file, "old").unwrap();
            let old = inode(&file);
            folder.save(&file, b"new").unwrap();
            assert_eq!(fs::read_to_string(&file).unwrap(), "new");
            assert_ne!(inode(&file), old);
            assert_eq!(folder.names(), ["notes.txt"]);
            assert!(folder.backups_left().is_empty());
        }
    }
}
