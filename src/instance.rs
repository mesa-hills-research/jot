//! One jot per user: launching jot while it runs hands the launch's files to
//! the running jot, and the launch exits.
//!
//! The first jot listens on an endpoint only its user can reach: a Unix
//! socket in a private folder, or on Windows a named pipe for the user's
//! login session. A later launch connects, sends a [`Request`] and waits for
//! the [`Reply`]. When the running jot doesn't answer in time, or declines,
//! the later launch runs on its own.
//!
//! Each message is a line of JSON with the [`PROTOCOL_VERSION`] it was
//! written for. A jot declines a request from a version it doesn't know.
//! Optional fields can be added to a version, since older readers skip
//! fields they don't know. A change an older jot would misread needs a new
//! version.

#[cfg(windows)]
mod pipe;
#[cfg(unix)]
mod socket;
#[cfg(test)]
mod tests;

#[cfg(windows)]
use pipe as platform;
#[cfg(unix)]
use socket as platform;

pub use platform::Endpoint;

use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The version of the messages this jot writes, and the one it reads.
pub const PROTOCOL_VERSION: u32 = 1;

/// The longest message read, so a stray connection can't fill memory.
const MAX_MESSAGE_BYTES: u64 = 1 << 20;

/// How often a launch tries again while a jot that started at the same
/// moment gets ready to listen.
const RETRY_INTERVAL: Duration = Duration::from_millis(20);

/// The longest the running jot waits for its windows to take a request,
/// whatever the launch asks for.
const MAX_ANSWER_WAIT: Duration = Duration::from_secs(30);

/// How long a launch waits for the running jot.
#[derive(Clone, Copy, Debug)]
pub struct Patience {
    /// For a jot that has the endpoint, such as one starting at the same
    /// moment, to listen.
    pub connect: Duration,
    /// For the running jot to take the request.
    pub answer: Duration,
}

impl Default for Patience {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(2),
            answer: Duration::from_secs(3),
        }
    }
}

/// How this process starts.
pub enum Startup {
    /// This is the first jot, which answers later launches.
    Primary(Server),
    /// The running jot took the files, so this process has nothing to do.
    HandedOver,
    /// This jot runs on its own: the running one didn't answer, declined,
    /// or the endpoint can't be used.
    Alone,
}

/// Hands `paths` to the user's running jot, or makes this process the one
/// that later launches hand theirs to.
pub fn start(paths: &[PathBuf]) -> Startup {
    match Endpoint::for_user() {
        Ok(endpoint) => start_at(&endpoint, paths, Patience::default()),
        Err(error) => {
            log::warn!("jot can\u{2019}t look for a running jot, so it runs on its own: {error}");
            Startup::Alone
        }
    }
}

/// [`start`] at `endpoint`.
pub fn start_at(endpoint: &Endpoint, paths: &[PathBuf], patience: Patience) -> Startup {
    let give_up = Instant::now() + patience.connect;
    loop {
        match platform::claim(endpoint) {
            Ok(Some(listener)) => return Startup::Primary(Server { listener }),
            Ok(None) => {}
            Err(error) => {
                log::warn!("jot can\u{2019}t use {endpoint}, so it runs on its own: {error}");
                return Startup::Alone;
            }
        }
        match platform::connect(endpoint) {
            Ok(Some(connection)) => return hand_over(connection, paths, patience.answer),
            Ok(None) => {}
            Err(error) => {
                log::warn!(
                    "jot can\u{2019}t reach the running jot at {endpoint}, so it runs on its \
                     own: {error}"
                );
                return Startup::Alone;
            }
        }
        if Instant::now() >= give_up {
            log::warn!("The running jot isn\u{2019}t listening, so this one runs on its own.");
            return Startup::Alone;
        }
        thread::sleep(RETRY_INTERVAL);
    }
}

/// Sends `paths` over `connection` and waits for the answer.
fn hand_over(connection: platform::Connection, paths: &[PathBuf], wait: Duration) -> Startup {
    let request = Request::new(paths, wait);
    // On a thread, so a jot that never answers can't hold the launch up.
    let (sender, answer) = mpsc::channel();
    thread::spawn(move || {
        let mut connection = connection;
        let reply = write_message(&mut connection, &request)
            .and_then(|()| read_message::<Reply>(&mut connection));
        sender.send(reply).ok();
    });
    match answer.recv_timeout(wait) {
        Ok(Ok(reply)) if reply.accepted => Startup::HandedOver,
        Ok(Ok(reply)) => {
            log::warn!(
                "The running jot declined the files, so this one runs on its own: {}",
                reply.error.as_deref().unwrap_or("no reason given")
            );
            Startup::Alone
        }
        Ok(Err(error)) => {
            log::warn!(
                "The running jot didn\u{2019}t answer, so this one runs on its own: {error}"
            );
            Startup::Alone
        }
        Err(_) => {
            log::warn!(
                "The running jot didn\u{2019}t answer within {} ms, so this one runs on its own.",
                wait.as_millis()
            );
            Startup::Alone
        }
    }
}

/// The first jot's end of the endpoint.
pub struct Server {
    listener: platform::Listener,
}

impl Server {
    /// Answers launches on a thread of its own, giving each request to
    /// `deliver`. The launch is told the request was taken once
    /// [`Incoming::accept`] is called.
    pub fn serve(self, deliver: impl Fn(Incoming) + Send + Sync + 'static) {
        let deliver = Arc::new(deliver);
        let mut listener = self.listener;
        let spawned = thread::Builder::new()
            .name("jot launches".into())
            .spawn(move || {
                let mut failures = 0;
                while failures < 10 {
                    match listener.accept() {
                        Ok(connection) => {
                            failures = 0;
                            let deliver = deliver.clone();
                            thread::spawn(move || answer(connection, &*deliver));
                        }
                        Err(error) => {
                            failures += 1;
                            log::error!(
                                "jot couldn\u{2019}t take a launch\u{2019}s request: {error}"
                            );
                            thread::sleep(Duration::from_millis(100));
                        }
                    }
                }
                log::error!("jot stopped answering later launches.");
            });
        if let Err(error) = spawned {
            log::error!("jot can\u{2019}t answer later launches: {error}");
        }
    }
}

/// Reads a request from `connection`, delivers it, and answers.
fn answer(mut connection: platform::Connection, deliver: &dyn Fn(Incoming)) {
    let reply = match read_request(&mut connection) {
        Ok(request) => match request.answer_by() {
            Some(deadline) => {
                let incoming = Incoming::new(request.paths());
                let answer = incoming.answer.clone();
                deliver(incoming);
                if answer.wait_until(deadline) {
                    Reply::accepted()
                } else {
                    Reply::declined("jot is busy.")
                }
            }
            None => Reply::declined("The request came too late."),
        },
        Err(error) => Reply::declined(error),
    };
    if let Err(error) = write_message(&mut connection, &reply) {
        log::debug!("Couldn\u{2019}t answer a launch: {error}");
    }
    platform::finish(connection);
}

fn read_request(connection: &mut impl Read) -> Result<Request, String> {
    let value: serde_json::Value = read_message(connection).map_err(|error| error.to_string())?;
    match value.get("version").and_then(serde_json::Value::as_u64) {
        Some(version) if version == u64::from(PROTOCOL_VERSION) => {}
        Some(version) => {
            return Err(format!(
                "This jot reads version {PROTOCOL_VERSION} requests, not version {version}."
            ));
        }
        None => return Err("The request has no version.".into()),
    }
    serde_json::from_value(value).map_err(|error| format!("The request is malformed: {error}"))
}

/// A request from a later launch, as the running jot receives it.
pub struct Incoming {
    paths: Vec<PathBuf>,
    answer: Arc<Answer>,
}

impl Incoming {
    fn new(paths: Vec<PathBuf>) -> Self {
        Self {
            paths,
            answer: Arc::default(),
        }
    }

    /// Takes the request on, which tells the launch that it can exit, and
    /// returns the files it asks to open. With none, it asks for a window.
    ///
    /// Returns `None` when the launch has stopped waiting and runs on its
    /// own, so the request is dropped.
    pub fn accept(mut self) -> Option<Vec<PathBuf>> {
        self.answer
            .accept()
            .then(|| std::mem::take(&mut self.paths))
    }
}

impl Drop for Incoming {
    /// A request dropped without being taken, such as while jot quits,
    /// lets the launch run on its own straight away.
    fn drop(&mut self) {
        self.answer.give_up();
    }
}

/// Whether the running jot's windows took a request, which the thread
/// answering the launch waits for.
#[derive(Default)]
struct Answer {
    state: Mutex<AnswerState>,
    changed: Condvar,
}

#[derive(Clone, Copy, Default, PartialEq)]
enum AnswerState {
    #[default]
    Waiting,
    Accepted,
    GaveUp,
}

impl Answer {
    /// Takes the request, unless the answering thread gave up on it.
    fn accept(&self) -> bool {
        self.settle(AnswerState::Accepted) == AnswerState::Accepted
    }

    fn give_up(&self) {
        self.settle(AnswerState::GaveUp);
    }

    /// Moves a waiting answer to `to`, and returns the answer.
    fn settle(&self, to: AnswerState) -> AnswerState {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if *state == AnswerState::Waiting {
            *state = to;
            self.changed.notify_all();
        }
        *state
    }

    /// Waits until the request is taken, and returns whether it was. At
    /// `deadline` it gives up, and a later [`Self::accept`] returns false.
    fn wait_until(&self, deadline: Instant) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        while *state == AnswerState::Waiting {
            let now = Instant::now();
            if now >= deadline {
                *state = AnswerState::GaveUp;
                break;
            }
            state = self
                .changed
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(|error| error.into_inner())
                .0;
        }
        *state == AnswerState::Accepted
    }
}

/// A launch's request: the files to open, as absolute paths. With none it
/// asks for a window.
#[derive(Debug, Serialize, Deserialize)]
struct Request {
    version: u32,
    paths: Vec<WirePath>,
    /// When the launch sent the request, in milliseconds since 1970.
    sent_at: u64,
    /// How long the launch waits for the answer, in milliseconds. The
    /// running jot gives up a little sooner, so it never opens files that
    /// the launch opens too.
    wait: u64,
}

impl Request {
    fn new(paths: &[PathBuf], wait: Duration) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            paths: paths
                .iter()
                .map(|path| WirePath::new(path.clone()))
                .collect(),
            sent_at: millis_since_epoch(SystemTime::now()),
            wait: u64::try_from(wait.as_millis()).unwrap_or(u64::MAX),
        }
    }

    fn paths(&self) -> Vec<PathBuf> {
        self.paths.iter().filter_map(WirePath::to_path).collect()
    }

    /// When to give up waiting for the windows to take the request, or
    /// `None` when the launch has already stopped waiting.
    fn answer_by(&self) -> Option<Instant> {
        let wait = Duration::from_millis(self.wait);
        let margin = (wait / 4).min(Duration::from_millis(500));
        let give_up_at =
            UNIX_EPOCH + Duration::from_millis(self.sent_at) + wait.saturating_sub(margin);
        let left = give_up_at.duration_since(SystemTime::now()).ok()?;
        Some(Instant::now() + left.min(MAX_ANSWER_WAIT))
    }
}

fn millis_since_epoch(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map(|since| u64::try_from(since.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

/// The running jot's answer.
#[derive(Debug, Serialize, Deserialize)]
struct Reply {
    version: u32,
    /// Whether the running jot took the request. Otherwise the launch runs
    /// on its own.
    accepted: bool,
    /// Why it declined.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl Reply {
    fn accepted() -> Self {
        Self {
            version: PROTOCOL_VERSION,
            accepted: true,
            error: None,
        }
    }

    fn declined(error: impl Into<String>) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            accepted: false,
            error: Some(error.into()),
        }
    }
}

/// A path in a message: text when it is Unicode, as nearly every path is,
/// and otherwise the platform's own units, bytes on Unix and UTF-16 on
/// Windows.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
enum WirePath {
    Text(String),
    Units(Vec<u16>),
}

impl WirePath {
    fn new(path: PathBuf) -> Self {
        match path.into_os_string().into_string() {
            Ok(text) => Self::Text(text),
            Err(os) => Self::Units(os_units(&os)),
        }
    }

    fn to_path(&self) -> Option<PathBuf> {
        match self {
            Self::Text(text) => Some(PathBuf::from(text)),
            Self::Units(units) => os_from_units(units).map(PathBuf::from),
        }
    }
}

#[cfg(unix)]
fn os_units(os: &OsString) -> Vec<u16> {
    use std::os::unix::ffi::OsStrExt;
    os.as_bytes().iter().copied().map(u16::from).collect()
}

#[cfg(unix)]
fn os_from_units(units: &[u16]) -> Option<OsString> {
    use std::os::unix::ffi::OsStringExt;
    let bytes = units
        .iter()
        .map(|&unit| u8::try_from(unit).ok())
        .collect::<Option<Vec<u8>>>()?;
    Some(OsString::from_vec(bytes))
}

#[cfg(windows)]
fn os_units(os: &OsString) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    os.encode_wide().collect()
}

#[cfg(windows)]
fn os_from_units(units: &[u16]) -> Option<OsString> {
    use std::os::windows::ffi::OsStringExt;
    Some(OsString::from_wide(units))
}

fn write_message(connection: &mut impl Write, message: &impl Serialize) -> io::Result<()> {
    let mut line = serde_json::to_vec(message)?;
    line.push(b'\n');
    connection.write_all(&line)?;
    connection.flush()
}

fn read_message<T: for<'de> Deserialize<'de>>(connection: &mut impl Read) -> io::Result<T> {
    let mut line = Vec::new();
    BufReader::new(connection.take(MAX_MESSAGE_BYTES)).read_until(b'\n', &mut line)?;
    if line.last() != Some(&b'\n') {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "the message ended early",
        ));
    }
    Ok(serde_json::from_slice(&line)?)
}
