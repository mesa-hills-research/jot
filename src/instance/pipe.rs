//! The endpoint on Windows: a named pipe for the user's login session that
//! only the user can open. Windows removes a pipe when the process that
//! made it ends, so a jot that crashed leaves nothing behind.

use std::fmt;
use std::fs::File;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use windows::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GENERIC_ALL,
    GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
};
use windows::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, AddAccessAllowedAce, GetLengthSid, GetTokenInformation,
    InitializeAcl, InitializeSecurityDescriptor, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES,
    SECURITY_DESCRIPTOR, SetSecurityDescriptorDacl, TOKEN_QUERY, TOKEN_USER, TokenSessionId,
    TokenUser,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_SHARE_NONE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
    SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeServerProcessId, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    WaitNamedPipeW,
};
use windows::Win32::System::SystemServices::SECURITY_DESCRIPTOR_REVISION;
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow;
use windows::core::PCWSTR;

/// The size of the pipe's buffers. A message is a short line of JSON.
const BUFFER_SIZE: u32 = 4096;

/// The pipe jot listens on, and the user it belongs to.
pub struct Endpoint {
    /// The pipe's name, ending in a nul.
    name: Vec<u16>,
    user: Sid,
}

impl Endpoint {
    /// `\\.\pipe\jot-<user's SID>-<session>`.
    pub fn for_user() -> io::Result<Self> {
        let token = Token::of(current_process())?;
        let user = token.user()?;
        let session = token.session()?;
        Ok(Self::with_name(&format!("jot-{user}-{session}"), user))
    }

    /// The pipe `\\.\pipe\<name>`, for this user.
    #[cfg(test)]
    pub fn named(name: &str) -> io::Result<Self> {
        let user = Token::of(current_process())?.user()?;
        Ok(Self::with_name(name, user))
    }

    fn with_name(name: &str, user: Sid) -> Self {
        let name = format!(r"\\.\pipe\{name}")
            .encode_utf16()
            .chain([0])
            .collect();
        Self { name, user }
    }

    fn name(&self) -> PCWSTR {
        PCWSTR(self.name.as_ptr())
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = &self.name[..self.name.len() - 1];
        write!(f, "{}", String::from_utf16_lossy(name))
    }
}

pub type Connection = File;

/// The pipe's waiting instance. Another one is made as each launch
/// connects, so the pipe stays this process's.
pub struct Listener {
    name: Vec<u16>,
    security: Security,
    waiting: Option<OwnedHandle>,
}

impl Listener {
    pub fn accept(&mut self) -> io::Result<Connection> {
        let waiting = self
            .waiting
            .as_ref()
            .ok_or_else(|| io::Error::other("the pipe is closed"))?;
        // SAFETY: the handle is a pipe instance this listener owns.
        let connected = unsafe { ConnectNamedPipe(handle(waiting), None) };
        // A launch that connected between making the instance and waiting
        // on it is connected too.
        let error = connected
            .err()
            .filter(|error| error.code() != ERROR_PIPE_CONNECTED.to_hresult());
        // The next instance comes first, so the pipe never goes away.
        let next = create_instance(&self.name, &self.security, false);
        if let Some(error) = error {
            if let Ok(next) = next {
                self.waiting = Some(next);
            }
            return Err(io::Error::other(error));
        }
        let connected = match next {
            Ok(next) => self.waiting.replace(next),
            Err(error) => {
                log::error!("jot can\u{2019}t keep its pipe open for later launches: {error}");
                self.waiting.take()
            }
        };
        Ok(File::from(connected.expect("an instance was waiting")))
    }
}

/// Makes this process the one that listens, unless the pipe exists: then
/// it returns `None`.
pub fn claim(endpoint: &Endpoint) -> io::Result<Option<Listener>> {
    let security = Security::for_user(&endpoint.user)?;
    match create_instance(&endpoint.name, &security, true) {
        Ok(waiting) => Ok(Some(Listener {
            name: endpoint.name.clone(),
            security,
            waiting: Some(waiting),
        })),
        // The first instance exists, so another jot listens.
        Err(error) if error.raw_os_error() == Some(ERROR_ACCESS_DENIED.0 as i32) => Ok(None),
        Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY.0 as i32) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Connects to the jot that listens, or returns `None` when none does yet.
/// The pipe has to be one this user's process made. The connection lets
/// that process come to the front.
pub fn connect(endpoint: &Endpoint) -> io::Result<Option<Connection>> {
    // The server may learn who connected, but can't act as them.
    // SAFETY: the name ends in a nul and outlives the call.
    let opened = unsafe {
        CreateFileW(
            endpoint.name(),
            (GENERIC_READ | GENERIC_WRITE).0,
            FILE_SHARE_NONE,
            None,
            OPEN_EXISTING,
            SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
            None,
        )
    };
    let pipe = match opened {
        // SAFETY: CreateFileW returned a handle that nothing else owns.
        Ok(pipe) => unsafe { OwnedHandle::from_raw_handle(pipe.0) },
        Err(error) if error.code() == ERROR_FILE_NOT_FOUND.to_hresult() => return Ok(None),
        Err(error) if error.code() == ERROR_PIPE_BUSY.to_hresult() => {
            // Every instance is busy: wait a moment for one.
            // SAFETY: the name ends in a nul and outlives the call.
            let _ = unsafe { WaitNamedPipeW(endpoint.name(), 100) };
            return Ok(None);
        }
        Err(error) => return Err(io::Error::other(error)),
    };

    let mut server = 0;
    // SAFETY: the handle is the client end of a pipe, and `server` is
    // writable.
    unsafe { GetNamedPipeServerProcessId(handle(&pipe), &mut server) }.map_err(io::Error::other)?;
    // SAFETY: OpenProcess takes any process id, and returns a new handle.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, server) }
        .map_err(io::Error::other)?;
    // SAFETY: the handle is new and nothing else owns it.
    let process = unsafe { OwnedHandle::from_raw_handle(process.0) };
    if Token::of(handle(&process))?.user()? != endpoint.user {
        return Err(io::Error::other("the pipe belongs to another user"));
    }
    // Windows brings a window to the front only for the process the user
    // is working in, which is this one. This passes that on.
    // SAFETY: any process id is allowed.
    let _ = unsafe { AllowSetForegroundWindow(server) };
    Ok(Some(File::from(pipe)))
}

/// Waits for the launch to read the answer, then closes the connection.
pub fn finish(connection: Connection) {
    // On a pipe, this waits until the other end has read everything.
    let _ = connection.sync_all();
}

fn create_instance(name: &[u16], security: &Security, first: bool) -> io::Result<OwnedHandle> {
    let mut open_mode = PIPE_ACCESS_DUPLEX;
    if first {
        open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    let attributes = security.attributes();
    // SAFETY: the name ends in a nul, and the attributes and the security
    // descriptor they point to outlive the call.
    let pipe = unsafe {
        CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            open_mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            BUFFER_SIZE,
            BUFFER_SIZE,
            0,
            Some(&attributes),
        )
    };
    if pipe == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateNamedPipeW returned a handle that nothing else owns.
    Ok(unsafe { OwnedHandle::from_raw_handle(pipe.0) })
}

fn handle(owned: &OwnedHandle) -> HANDLE {
    HANDLE(owned.as_raw_handle())
}

fn current_process() -> HANDLE {
    // SAFETY: GetCurrentProcess has no preconditions.
    unsafe { GetCurrentProcess() }
}

/// A security identifier, kept in a buffer aligned for one.
#[derive(Clone)]
struct Sid {
    words: Vec<u32>,
    len: usize,
}

impl Sid {
    /// Copies the SID at `sid`.
    ///
    /// # Safety
    ///
    /// `sid` must point to a valid SID.
    unsafe fn copy(sid: PSID) -> Self {
        // SAFETY: the caller passes a valid SID, `len` bytes long.
        let len = unsafe { GetLengthSid(sid) } as usize;
        let mut words = vec![0u32; len.div_ceil(4)];
        // SAFETY: both buffers hold `len` bytes, and don't overlap.
        unsafe {
            std::ptr::copy_nonoverlapping(sid.0 as *const u8, words.as_mut_ptr().cast(), len);
        }
        Self { words, len }
    }

    fn bytes(&self) -> &[u8] {
        // SAFETY: `words` holds at least `len` initialized bytes.
        unsafe { std::slice::from_raw_parts(self.words.as_ptr().cast(), self.len) }
    }

    fn as_psid(&self) -> PSID {
        PSID(self.words.as_ptr() as *mut _)
    }
}

impl PartialEq for Sid {
    fn eq(&self, other: &Self) -> bool {
        self.bytes() == other.bytes()
    }
}

/// The SID's usual text form, such as `S-1-5-21-…-1001`.
impl fmt::Display for Sid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bytes = self.bytes();
        let (Some(&revision), Some(&count), Some(authority)) =
            (bytes.first(), bytes.get(1), bytes.get(2..8))
        else {
            return write!(f, "S-?");
        };
        let authority = authority
            .iter()
            .fold(0u64, |value, &byte| (value << 8) | u64::from(byte));
        write!(f, "S-{revision}-{authority}")?;
        for sub in bytes[8..].chunks_exact(4).take(usize::from(count)) {
            write!(
                f,
                "-{}",
                u32::from_le_bytes([sub[0], sub[1], sub[2], sub[3]])
            )?;
        }
        Ok(())
    }
}

/// A process's access token.
struct Token(OwnedHandle);

impl Token {
    fn of(process: HANDLE) -> io::Result<Self> {
        let mut token = HANDLE::default();
        // SAFETY: `process` is a process handle, and `token` is writable.
        unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) }.map_err(io::Error::other)?;
        // SAFETY: the token handle is new and nothing else owns it.
        Ok(Self(unsafe { OwnedHandle::from_raw_handle(token.0) }))
    }

    /// The user the process runs as.
    fn user(&self) -> io::Result<Sid> {
        let mut len = 0;
        // SAFETY: asks for the size only.
        let _ = unsafe { GetTokenInformation(handle(&self.0), TokenUser, None, 0, &mut len) };
        let mut buffer = vec![0u64; (len as usize).div_ceil(8)];
        // SAFETY: the buffer holds `len` bytes, aligned for a TOKEN_USER.
        unsafe {
            GetTokenInformation(
                handle(&self.0),
                TokenUser,
                Some(buffer.as_mut_ptr().cast()),
                len,
                &mut len,
            )
        }
        .map_err(io::Error::other)?;
        // SAFETY: GetTokenInformation filled the buffer with a TOKEN_USER,
        // whose SID points into the buffer.
        Ok(unsafe { Sid::copy((*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid) })
    }

    /// The process's login session.
    fn session(&self) -> io::Result<u32> {
        let mut session = 0u32;
        let mut len = 0;
        // SAFETY: `session` holds the u32 asked for.
        unsafe {
            GetTokenInformation(
                handle(&self.0),
                TokenSessionId,
                Some((&mut session as *mut u32).cast()),
                size_of::<u32>() as u32,
                &mut len,
            )
        }
        .map_err(io::Error::other)?;
        Ok(session)
    }
}

/// A security descriptor that lets only one user open the pipe.
struct Security {
    descriptor: Box<SECURITY_DESCRIPTOR>,
    /// The descriptor's access list, which it points to.
    _acl: Vec<u32>,
}

// SAFETY: the descriptor's pointers point into the access list it owns, and
// nothing changes either once they are made.
unsafe impl Send for Security {}

impl Security {
    fn for_user(user: &Sid) -> io::Result<Self> {
        let acl_len =
            size_of::<ACL>() + size_of::<ACCESS_ALLOWED_ACE>() - size_of::<u32>() + user.len;
        let mut acl = vec![0u32; acl_len.div_ceil(4)];
        let acl_ptr = acl.as_mut_ptr().cast::<ACL>();
        let mut descriptor = Box::new(SECURITY_DESCRIPTOR::default());
        let descriptor_ptr =
            PSECURITY_DESCRIPTOR((&mut *descriptor as *mut SECURITY_DESCRIPTOR).cast());
        // SAFETY: the access list's buffer is `acl_len` bytes, aligned for an
        // ACL, the SID is valid, and the descriptor is a writable
        // SECURITY_DESCRIPTOR. Both outlive their use, in `Self`.
        unsafe {
            InitializeAcl(acl_ptr, acl_len as u32, ACL_REVISION).map_err(io::Error::other)?;
            AddAccessAllowedAce(acl_ptr, ACL_REVISION, GENERIC_ALL.0, user.as_psid())
                .map_err(io::Error::other)?;
            InitializeSecurityDescriptor(descriptor_ptr, SECURITY_DESCRIPTOR_REVISION)
                .map_err(io::Error::other)?;
            SetSecurityDescriptorDacl(descriptor_ptr, true, Some(acl_ptr.cast_const()), false)
                .map_err(io::Error::other)?;
        }
        Ok(Self {
            descriptor,
            _acl: acl,
        })
    }

    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: (&*self.descriptor as *const SECURITY_DESCRIPTOR)
                .cast_mut()
                .cast(),
            bInheritHandle: false.into(),
        }
    }
}
