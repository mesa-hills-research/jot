//! jot's command line: `jot [--new-instance] [--] [FILE]...`
//!
//! Each file opens in a tab. `--new-instance` starts a jot of its own even
//! while one runs, and later launches don't reach it, which is useful for
//! testing. Options jot doesn't know are skipped, such as the `-psn_…` that
//! older macOS versions pass, and `--` ends the options.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

#[derive(Debug, Default, PartialEq)]
pub struct Args {
    /// The files to open, as absolute paths.
    pub paths: Vec<PathBuf>,
    /// Run apart from any jot that is running.
    pub new_instance: bool,
}

impl Args {
    /// Reads the arguments after the program's name. Relative paths are
    /// taken from `cwd`, the folder jot was started in.
    pub fn parse(args: impl IntoIterator<Item = OsString>, cwd: &Path) -> Self {
        let mut parsed = Self::default();
        let mut options_ended = false;
        for arg in args {
            if !options_ended {
                if arg == "--" {
                    options_ended = true;
                    continue;
                }
                if arg == "--new-instance" {
                    parsed.new_instance = true;
                    continue;
                }
                if is_option(&arg) {
                    log::warn!("jot skips the option {}", arg.to_string_lossy());
                    continue;
                }
            }
            parsed.paths.push(resolve(&arg, cwd));
        }
        parsed
    }
}

/// Whether `arg` looks like an option. A lone `-` is a file name.
fn is_option(arg: &OsStr) -> bool {
    let bytes = arg.as_encoded_bytes();
    bytes.len() > 1 && bytes[0] == b'-'
}

/// The absolute path of the file `arg` names, relative to `cwd`. A
/// `file:` URL, as some file managers pass, names the file at its path.
fn resolve(arg: &OsStr, cwd: &Path) -> PathBuf {
    if let Some(path) = arg.to_str().and_then(file_url_to_path) {
        return path;
    }
    let joined = cwd.join(arg);
    std::path::absolute(&joined).unwrap_or(joined)
}

/// The path of a `file:` URL, such as the ones macOS opens files with, or
/// `None` for any other URL.
pub fn file_url_to_path(url: &str) -> Option<PathBuf> {
    let scheme_end = url.find(':')?;
    if !url[..scheme_end].eq_ignore_ascii_case("file") {
        return None;
    }
    let rest = url[scheme_end + 1..].strip_prefix("//")?;
    // The host, empty or `localhost` for this computer.
    let path_start = rest.find('/')?;
    let (host, path) = rest.split_at(path_start);
    let path = path.split(['?', '#']).next().unwrap_or_default();
    let bytes = percent_decode(path)?;
    url_path(host, bytes)
}

#[cfg(unix)]
fn url_path(host: &str, bytes: Vec<u8>) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    if !host.is_empty() && !host.eq_ignore_ascii_case("localhost") {
        return None;
    }
    Some(PathBuf::from(OsString::from_vec(bytes)))
}

#[cfg(windows)]
fn url_path(host: &str, bytes: Vec<u8>) -> Option<PathBuf> {
    let path = String::from_utf8(bytes).ok()?.replace('/', "\\");
    if !host.is_empty() && !host.eq_ignore_ascii_case("localhost") {
        // A file on another computer: `\\host\share\…`.
        return Some(PathBuf::from(format!(r"\\{host}{path}")));
    }
    // `/C:/Users/…` is `C:\Users\…`.
    let path = path.strip_prefix('\\').unwrap_or(&path);
    Some(PathBuf::from(path))
}

/// Decodes the `%xx` escapes of `text`, or returns `None` for a broken one.
fn percent_decode(text: &str) -> Option<Vec<u8>> {
    let mut bytes = Vec::with_capacity(text.len());
    let mut rest = text.as_bytes();
    while let Some((&byte, after)) = rest.split_first() {
        if byte == b'%' {
            let hex = after.get(..2)?;
            let hex = std::str::from_utf8(hex).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            rest = &after[2..];
        } else {
            bytes.push(byte);
            rest = after;
        }
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str], cwd: &Path) -> Args {
        Args::parse(args.iter().map(OsString::from), cwd)
    }

    fn cwd() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(r"C:\Users\me\notes")
        } else {
            PathBuf::from("/home/me/notes")
        }
    }

    #[test]
    fn no_arguments_open_nothing() {
        assert_eq!(parse(&[], &cwd()), Args::default());
    }

    #[test]
    fn relative_paths_are_taken_from_the_folder_jot_started_in() {
        let args = parse(&["a.txt", "./drafts/b.md"], &cwd());
        assert_eq!(
            args.paths,
            vec![cwd().join("a.txt"), cwd().join("drafts").join("b.md")]
        );
        assert!(!args.new_instance);
    }

    #[test]
    fn absolute_paths_stay() {
        let absolute = if cfg!(windows) {
            r"D:\letters\c.txt"
        } else {
            "/srv/letters/c.txt"
        };
        assert_eq!(
            parse(&[absolute], &cwd()).paths,
            vec![PathBuf::from(absolute)]
        );
    }

    #[test]
    fn new_instance_is_an_option_anywhere_before_the_files_end() {
        let args = parse(&["a.txt", "--new-instance"], &cwd());
        assert!(args.new_instance);
        assert_eq!(args.paths, vec![cwd().join("a.txt")]);
    }

    #[test]
    fn unknown_options_are_skipped_and_a_dash_is_a_file() {
        let args = parse(&["-psn_0_1234", "--verbose", "-", "a.txt"], &cwd());
        assert_eq!(args.paths, vec![cwd().join("-"), cwd().join("a.txt")]);
    }

    #[test]
    fn after_two_dashes_everything_is_a_file() {
        let args = parse(&["--", "--new-instance", "-x.txt"], &cwd());
        assert!(!args.new_instance);
        assert_eq!(
            args.paths,
            vec![cwd().join("--new-instance"), cwd().join("-x.txt")]
        );
    }

    #[cfg(unix)]
    #[test]
    fn file_urls_name_their_files() {
        assert_eq!(
            file_url_to_path("file:///home/me/my%20notes/caf%C3%A9.txt"),
            Some(PathBuf::from("/home/me/my notes/café.txt"))
        );
        assert_eq!(
            file_url_to_path("file://localhost/tmp/a.txt"),
            Some(PathBuf::from("/tmp/a.txt"))
        );
        assert_eq!(file_url_to_path("file://server/tmp/a.txt"), None);
        assert_eq!(file_url_to_path("https://example.com/a.txt"), None);
        assert_eq!(file_url_to_path("file:///broken%2"), None);
        assert_eq!(
            parse(&["file:///home/me/a%23.txt"], &cwd()).paths,
            vec![PathBuf::from("/home/me/a#.txt")]
        );
    }

    #[cfg(windows)]
    #[test]
    fn file_urls_name_their_files() {
        assert_eq!(
            file_url_to_path("file:///C:/Users/me/my%20notes.txt"),
            Some(PathBuf::from(r"C:\Users\me\my notes.txt"))
        );
        assert_eq!(
            file_url_to_path("file://server/share/a.txt"),
            Some(PathBuf::from(r"\\server\share\a.txt"))
        );
    }
}
