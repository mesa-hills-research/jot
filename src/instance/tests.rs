//! A running jot and later launches, as a server and clients in one
//! process.

use super::*;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Barrier, mpsc::Receiver};

/// Quick to give up, so the tests that wait for a silent jot are short.
const QUICK: Patience = Patience {
    connect: Duration::from_millis(500),
    answer: Duration::from_millis(600),
};

/// A fresh endpoint, removed afterwards.
struct Scratch {
    endpoint: Endpoint,
    #[cfg(unix)]
    dir: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let name = format!(
            "jot-instance-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        #[cfg(unix)]
        {
            let dir = std::env::temp_dir().join(name);
            std::fs::create_dir_all(&dir).unwrap();
            Self {
                endpoint: Endpoint::in_dir(dir.join("jot")),
                dir,
            }
        }
        #[cfg(windows)]
        {
            Self {
                endpoint: Endpoint::named(&name).unwrap(),
            }
        }
    }
}

#[cfg(unix)]
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Makes this the running jot, which takes every request and passes on
/// the paths.
fn serve(endpoint: &Endpoint) -> Receiver<Vec<PathBuf>> {
    let Startup::Primary(server) = start_at(endpoint, &[], Kind::Open, QUICK) else {
        panic!("the first jot should listen");
    };
    let (sender, received) = mpsc::channel();
    let sender = Mutex::new(sender);
    server.serve(move |incoming| {
        if let Some(paths) = incoming.accept() {
            sender.lock().unwrap().send(paths).unwrap();
        }
    });
    received
}

fn paths(names: &[&str]) -> Vec<PathBuf> {
    let root = if cfg!(windows) { r"C:\notes" } else { "/notes" };
    names
        .iter()
        .map(|name| Path::new(root).join(name))
        .collect()
}

fn outcome(startup: Startup) -> &'static str {
    match startup {
        Startup::Primary(_) => "primary",
        Startup::HandedOver => "handed over",
        Startup::Alone => "alone",
    }
}

#[test]
fn a_later_launch_hands_its_files_over() {
    let scratch = Scratch::new();
    let received = serve(&scratch.endpoint);

    let files = paths(&["a.txt", "b c.md"]);
    let startup = start_at(&scratch.endpoint, &files, Kind::Open, QUICK);
    assert_eq!(outcome(startup), "handed over");
    assert_eq!(received.recv().unwrap(), files);

    // Without files, it asks for a window.
    let startup = start_at(&scratch.endpoint, &[], Kind::Open, QUICK);
    assert_eq!(outcome(startup), "handed over");
    assert_eq!(received.recv().unwrap(), Vec::<PathBuf>::new());
}

#[cfg(unix)]
#[test]
fn a_path_that_is_not_unicode_arrives_whole() {
    use std::os::unix::ffi::OsStringExt;
    let scratch = Scratch::new();
    let received = serve(&scratch.endpoint);

    let file = PathBuf::from(OsString::from_vec(b"/notes/caf\xe9.txt".to_vec()));
    let startup = start_at(
        &scratch.endpoint,
        std::slice::from_ref(&file),
        Kind::Open,
        QUICK,
    );
    assert_eq!(outcome(startup), "handed over");
    assert_eq!(received.recv().unwrap(), vec![file]);
}

#[cfg(unix)]
#[test]
fn a_socket_left_by_a_crashed_jot_is_taken_over() {
    let scratch = Scratch::new();
    // A jot that crashed leaves its socket and lock file behind, and the
    // lock goes with the process.
    let Startup::Primary(server) = start_at(&scratch.endpoint, &[], Kind::Open, QUICK) else {
        panic!("the first jot should listen");
    };
    drop(server);
    assert!(scratch.endpoint.socket().exists());

    let received = serve(&scratch.endpoint);
    let files = paths(&["a.txt"]);
    let startup = start_at(&scratch.endpoint, &files, Kind::Open, QUICK);
    assert_eq!(outcome(startup), "handed over");
    assert_eq!(received.recv().unwrap(), files);
}

#[test]
fn of_launches_at_the_same_moment_exactly_one_listens() {
    const LAUNCHES: usize = 8;
    let scratch = Arc::new(Scratch::new());
    let barrier = Arc::new(Barrier::new(LAUNCHES));
    let (sender, received) = mpsc::channel();
    let launches: Vec<_> = (0..LAUNCHES)
        .map(|launch| {
            let (scratch, barrier, sender) = (scratch.clone(), barrier.clone(), sender.clone());
            thread::spawn(move || {
                barrier.wait();
                let files = paths(&[&format!("{launch}.txt")]);
                let startup = start_at(&scratch.endpoint, &files, Kind::Open, QUICK);
                let outcome = outcome_of(startup, sender);
                (outcome, files)
            })
        })
        .collect();
    drop(sender);

    let mut primaries = 0;
    let mut handed_over = Vec::new();
    for launch in launches {
        match launch.join().unwrap() {
            ("primary", _) => primaries += 1,
            ("handed over", files) => handed_over.extend(files),
            (outcome, files) => panic!("{files:?}: {outcome}"),
        }
    }
    assert_eq!(primaries, 1);
    assert_eq!(handed_over.len(), LAUNCHES - 1);
    // The running jot keeps its sender, so this takes as many as were
    // handed over.
    let mut taken: Vec<PathBuf> = (0..handed_over.len())
        .flat_map(|_| received.recv_timeout(Duration::from_secs(5)).unwrap())
        .collect();
    taken.sort();
    handed_over.sort();
    assert_eq!(taken, handed_over);

    /// Serves when `startup` is the primary, passing the paths it takes to
    /// `sender`.
    fn outcome_of(startup: Startup, sender: mpsc::Sender<Vec<PathBuf>>) -> &'static str {
        if let Startup::Primary(server) = startup {
            let sender = Mutex::new(sender);
            server.serve(move |incoming| {
                if let Some(paths) = incoming.accept() {
                    sender.lock().unwrap().send(paths).ok();
                }
            });
            "primary"
        } else {
            outcome(startup)
        }
    }
}

#[test]
fn a_jot_that_never_answers_lets_the_launch_run_alone() {
    let scratch = Scratch::new();
    // The endpoint is taken, but nothing reads from it.
    let Startup::Primary(_hung) = start_at(&scratch.endpoint, &[], Kind::Open, QUICK) else {
        panic!("the first jot should listen");
    };

    let started = Instant::now();
    let startup = start_at(&scratch.endpoint, &paths(&["a.txt"]), Kind::Open, QUICK);
    assert_eq!(outcome(startup), "alone");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn a_request_the_windows_never_take_is_declined() {
    let scratch = Scratch::new();
    let Startup::Primary(server) = start_at(&scratch.endpoint, &[], Kind::Open, QUICK) else {
        panic!("the first jot should listen");
    };
    // The windows are busy: requests wait, untaken.
    let (sender, held) = mpsc::channel();
    let sender = Mutex::new(sender);
    server.serve(move |incoming| sender.lock().unwrap().send(incoming).unwrap());

    let startup = start_at(&scratch.endpoint, &paths(&["a.txt"]), Kind::Open, QUICK);
    assert_eq!(outcome(startup), "alone");
    // Once the windows get to it, the launch has opened it itself.
    let incoming = held.recv().unwrap();
    assert_eq!(incoming.accept(), None);
}

#[test]
fn a_request_from_another_version_is_declined() {
    let scratch = Scratch::new();
    let received = serve(&scratch.endpoint);

    let mut future = Request::new(&paths(&["a.txt"]), Kind::Open, QUICK.answer);
    future.version = PROTOCOL_VERSION + 1;
    let reply = exchange(&scratch.endpoint, &serde_json::to_value(&future).unwrap());
    assert!(!reply.accepted);
    assert!(reply.error.unwrap().contains("version"));

    let reply = exchange(&scratch.endpoint, &serde_json::json!({"paths": ["/a.txt"]}));
    assert!(!reply.accepted);

    let reply = exchange(&scratch.endpoint, &serde_json::json!("hello"));
    assert!(!reply.accepted);

    // None of them reached the windows, and a good one still does.
    let files = paths(&["b.txt"]);
    assert_eq!(
        outcome(start_at(&scratch.endpoint, &files, Kind::Open, QUICK)),
        "handed over"
    );
    assert_eq!(received.recv().unwrap(), files);
}

#[test]
fn a_request_with_fields_this_version_lacks_is_read() {
    let scratch = Scratch::new();
    let received = serve(&scratch.endpoint);

    let mut request =
        serde_json::to_value(Request::new(&paths(&["a.txt"]), Kind::Open, QUICK.answer)).unwrap();
    request["line"] = 12.into();
    let reply = exchange(&scratch.endpoint, &request);
    assert!(reply.accepted, "{reply:?}");
    assert_eq!(received.recv().unwrap(), paths(&["a.txt"]));
}

#[test]
fn a_launch_can_ask_for_a_new_document() {
    let scratch = Scratch::new();
    let Startup::Primary(server) = start_at(&scratch.endpoint, &[], Kind::Open, QUICK) else {
        panic!("the first jot should listen");
    };
    let (sender, received) = mpsc::channel();
    let sender = Mutex::new(sender);
    server.serve(move |incoming| {
        let kind = incoming.kind();
        if let Some(paths) = incoming.accept() {
            sender.lock().unwrap().send((kind, paths)).unwrap();
        }
    });

    let startup = start_at(&scratch.endpoint, &[], Kind::NewDocument, QUICK);
    assert_eq!(outcome(startup), "handed over");
    assert_eq!(received.recv().unwrap(), (Kind::NewDocument, Vec::new()));

    let files = paths(&["a.txt"]);
    let startup = start_at(&scratch.endpoint, &files, Kind::Open, QUICK);
    assert_eq!(outcome(startup), "handed over");
    assert_eq!(received.recv().unwrap(), (Kind::Open, files));
}

#[test]
fn a_version_1_jot_takes_files_and_declines_a_new_document() {
    // Files go as version 1, without a kind, as they always have.
    let files = Request::new(&paths(&["a.txt"]), Kind::Open, QUICK.answer);
    let files = serde_json::to_value(files).unwrap();
    assert_eq!(files["version"], 1);
    assert!(files.get("kind").is_none(), "{files}");
    let line = format!("{files}\n");
    assert!(read_request(&mut line.as_bytes(), 1).is_ok());

    // A new document needs version 2, which a version 1 jot declines, so
    // the launch runs on its own.
    let new_document = Request::new(&[], Kind::NewDocument, QUICK.answer);
    let new_document = serde_json::to_value(new_document).unwrap();
    assert_eq!(new_document["version"], 2);
    let line = format!("{new_document}\n");
    let error = read_request(&mut line.as_bytes(), 1).unwrap_err();
    assert!(error.contains("version 2"), "{error}");
    let request = read_request(&mut line.as_bytes(), PROTOCOL_VERSION).unwrap();
    assert_eq!(request.kind, Kind::NewDocument);
}

#[test]
fn a_request_the_launch_stopped_waiting_for_is_dropped() {
    let mut request = Request::new(&paths(&["a.txt"]), Kind::Open, Duration::from_secs(3));
    assert!(request.answer_by().is_some());
    request.sent_at -= 3_000;
    assert!(request.answer_by().is_none());
}

#[cfg(unix)]
#[test]
fn a_folder_others_can_open_is_not_used() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = Scratch::new();
    let dir = scratch.dir.join("jot");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();

    let startup = start_at(&scratch.endpoint, &[], Kind::Open, QUICK);
    assert_eq!(outcome(startup), "alone");
}

/// Sends `message` as a request and reads the reply.
fn exchange(endpoint: &Endpoint, message: &serde_json::Value) -> Reply {
    let mut connection = platform::connect(endpoint).unwrap().unwrap();
    write_message(&mut connection, message).unwrap();
    read_message(&mut connection).unwrap()
}
