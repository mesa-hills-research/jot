//! The endpoint on Unix: a socket in a folder only the user can open. A
//! lock file beside it says which jot owns the socket, and the lock goes
//! with the process, so a socket left by a jot that crashed is replaced.

use std::fmt;
use std::fs::{self, DirBuilder, File, OpenOptions, TryLockError};
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long either end waits on the other's half of a message.
const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// Where jot listens: a folder holding the socket and its lock.
pub struct Endpoint {
    dir: PathBuf,
}

impl Endpoint {
    /// `$XDG_RUNTIME_DIR/jot`, or `jot-<uid>` in the temporary folder
    /// without a runtime folder.
    pub fn for_user() -> io::Result<Self> {
        let dir = match std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) {
            Some(runtime) if runtime.is_absolute() && runtime.is_dir() => runtime.join("jot"),
            _ => std::env::temp_dir().join(format!("jot-{}", user_id())),
        };
        Ok(Self { dir })
    }

    /// The endpoint in the folder `dir`, which is made if it is missing.
    #[cfg(test)]
    pub fn in_dir(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub(super) fn socket(&self) -> PathBuf {
        self.dir.join("jot.sock")
    }

    fn lock(&self) -> PathBuf {
        self.dir.join("jot.lock")
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.socket().display())
    }
}

pub type Connection = UnixStream;

/// The socket, with the lock that makes it this process's.
pub struct Listener {
    listener: UnixListener,
    _lock: File,
}

impl Listener {
    pub fn accept(&mut self) -> io::Result<Connection> {
        let (connection, _) = self.listener.accept()?;
        set_timeouts(&connection)?;
        Ok(connection)
    }
}

/// Makes this process the one that listens, unless another jot holds the
/// lock: then it returns `None`.
pub fn claim(endpoint: &Endpoint) -> io::Result<Option<Listener>> {
    make_private_dir(&endpoint.dir)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(endpoint.lock())?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Ok(None),
        Err(TryLockError::Error(error)) => return Err(error),
    }
    // With the lock, a socket here is one a jot that has quit or crashed
    // left behind.
    match fs::remove_file(endpoint.socket()) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let listener = UnixListener::bind(endpoint.socket())?;
    Ok(Some(Listener {
        listener,
        _lock: lock,
    }))
}

/// Connects to the jot that listens, or returns `None` when none does yet.
pub fn connect(endpoint: &Endpoint) -> io::Result<Option<Connection>> {
    match UnixStream::connect(endpoint.socket()) {
        Ok(connection) => {
            set_timeouts(&connection)?;
            Ok(Some(connection))
        }
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound
                    | io::ErrorKind::ConnectionRefused
                    | io::ErrorKind::WouldBlock
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// Closes the connection once the answer is written.
pub fn finish(_connection: Connection) {}

fn set_timeouts(connection: &UnixStream) -> io::Result<()> {
    connection.set_read_timeout(Some(IO_TIMEOUT))?;
    connection.set_write_timeout(Some(IO_TIMEOUT))
}

/// Makes the folder `dir` if it is missing, and checks that only this user
/// can open it, so no one else can listen there or reach the socket.
fn make_private_dir(dir: &Path) -> io::Result<()> {
    match DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let metadata = fs::symlink_metadata(dir)?;
    if !metadata.is_dir() || metadata.uid() != user_id() || metadata.mode() & 0o077 != 0 {
        return Err(io::Error::other(format!(
            "{} isn\u{2019}t a folder that only this user can open",
            dir.display()
        )));
    }
    Ok(())
}

fn user_id() -> u32 {
    // SAFETY: getuid has no preconditions and can't fail.
    unsafe { libc::getuid() }
}
