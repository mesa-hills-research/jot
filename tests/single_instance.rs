//! Launching the jot binary while jot runs hands the files on the command
//! line to the running jot, and the launch exits.
#![cfg(unix)]

use std::fs::{DirBuilder, File};
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const DEADLINE: Duration = Duration::from_secs(20);

/// A folder for one test, removed afterwards.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("jot-launch-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(dir.join("run"))
            .unwrap();
        DirBuilder::new()
            .recursive(true)
            .create(dir.join("work"))
            .unwrap();
        Self(dir)
    }

    /// The runtime folder jot looks for the running jot in.
    fn run(&self) -> PathBuf {
        self.0.join("run")
    }

    /// The folder jot is launched in.
    fn work(&self) -> PathBuf {
        self.0.join("work")
    }

    /// Launches jot with `args`.
    fn launch(&self, args: &[&str]) -> Child {
        Command::new(env!("CARGO_BIN_EXE_jot"))
            .args(args)
            .current_dir(self.work())
            .env("XDG_RUNTIME_DIR", self.run())
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            // A jot that didn't hand its files over has no display to open a
            // window on.
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A stand-in for a running jot, listening where jot looks for one, written
/// from the message format rather than jot's code.
struct RunningJot {
    listener: UnixListener,
    _lock: File,
}

impl RunningJot {
    fn start(run: &Path) -> Self {
        let dir = run.join("jot");
        DirBuilder::new().mode(0o700).create(&dir).unwrap();
        let lock = File::create(dir.join("jot.lock")).unwrap();
        lock.try_lock().unwrap();
        let listener = UnixListener::bind(dir.join("jot.sock")).unwrap();
        listener.set_nonblocking(true).unwrap();
        Self {
            listener,
            _lock: lock,
        }
    }

    /// Takes the next launch's request, and returns it.
    fn take_request(&self) -> serde_json::Value {
        let started = Instant::now();
        let mut stream = loop {
            match self.listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    assert!(started.elapsed() < DEADLINE, "jot never connected");
                    thread::sleep(Duration::from_millis(20));
                }
                Err(error) => panic!("{error}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream.set_read_timeout(Some(DEADLINE)).unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        stream
            .write_all(b"{\"version\":1,\"accepted\":true}\n")
            .unwrap();
        serde_json::from_str(&line).unwrap()
    }
}

fn wait(mut child: Child) -> ExitStatus {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if started.elapsed() > DEADLINE {
            child.kill().ok();
            panic!("jot didn't exit");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn paths(request: &serde_json::Value) -> Vec<PathBuf> {
    request["paths"]
        .as_array()
        .unwrap()
        .iter()
        .map(|path| PathBuf::from(path.as_str().unwrap()))
        .collect()
}

#[test]
fn a_launch_hands_its_files_to_the_running_jot_and_exits() {
    let scratch = Scratch::new("files");
    let running = RunningJot::start(&scratch.run());

    let launch = scratch.launch(&["notes.txt", "drafts/b.md", "--", "-dash.txt"]);
    let request = running.take_request();
    assert!(wait(launch).success());

    assert_eq!(request["version"], 1);
    let work = scratch.work();
    assert_eq!(
        paths(&request),
        [
            work.join("notes.txt"),
            work.join("drafts/b.md"),
            work.join("-dash.txt")
        ]
    );
}

#[test]
fn a_launch_without_files_asks_for_a_window() {
    let scratch = Scratch::new("window");
    let running = RunningJot::start(&scratch.run());

    let launch = scratch.launch(&[]);
    let request = running.take_request();
    assert!(wait(launch).success());
    assert_eq!(paths(&request), Vec::<PathBuf>::new());
}
