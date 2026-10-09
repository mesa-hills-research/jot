//! The registry keys that list jot in File Explorer's Open with menu and in
//! Settings > Default apps for text files, and the changes that take them
//! out again.
//!
//! Everything goes under `HKEY_CURRENT_USER`, for the user alone, so no
//! administrator is needed. Which app opens a type by default stays the
//! user's choice, made in Windows.

use super::{APP_ID, APP_NAME, same_path};
use std::path::{Path, PathBuf};

/// The ProgID of the files jot opens, which Open with lists jot through.
pub const PROG_ID: &str = "MesaHillsResearch.Jot.Document";

/// The types jot offers to open: text that people read and write by hand.
/// jot shows every file as plain text, without syntax highlighting, so
/// source code is left to code editors.
pub const EXTENSIONS: &[&str] = &[
    // Notes and prose.
    ".txt",
    ".md",
    ".markdown",
    ".log",
    // Settings and data.
    ".json",
    ".yaml",
    ".yml",
    ".toml",
    ".ini",
    ".cfg",
    ".conf",
    ".xml",
    // Tables.
    ".csv",
    ".tsv",
];

const CLASSES: &str = r"Software\Classes";
const VENDOR: &str = r"Software\MesaHillsResearch";
const APP: &str = r"Software\MesaHillsResearch\Jot";
const CAPABILITIES: &str = r"Software\MesaHillsResearch\Jot\Capabilities";
const REGISTERED_APPLICATIONS: &str = r"Software\RegisteredApplications";

/// The name File Explorer gives the types when jot is their default app.
const TYPE_NAME: &str = "Text Document";

/// What Default apps says about jot.
const DESCRIPTION: &str = "A fast, native text editor";

/// A change to a key under `HKEY_CURRENT_USER`. The value named "" is the
/// key's default value.
#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    /// Sets a value, making the key and those above it as needed.
    Set {
        key: String,
        name: String,
        value: Value,
    },
    /// Deletes a value. A value or key that isn't there is fine.
    DeleteValue { key: String, name: String },
    /// Deletes a key and everything in it.
    DeleteTree { key: String },
    /// Deletes a key that holds nothing, such as one made for a value
    /// that is gone.
    DeleteIfEmpty { key: String },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// A string, `REG_SZ`.
    Text(String),
    /// No data, `REG_NONE`, as `OpenWithProgids` takes.
    Empty,
}

/// What an earlier Add left in the registry.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Installed {
    /// The program the ProgID opens files with, when its command reads as
    /// one jot wrote.
    pub exe: Option<PathBuf>,
    /// The types Default apps lists for jot.
    pub extensions: Vec<String>,
}

/// The key of the ProgID's command, which names the program. jot is in
/// Open with when it is there.
pub fn command_key() -> String {
    format!(r"{CLASSES}\{PROG_ID}\shell\open\command")
}

/// The key that lists jot's types for Default apps.
pub fn file_associations_key() -> String {
    format!(r"{CAPABILITIES}\FileAssociations")
}

/// The command that opens a file with the jot at `exe`.
fn command(exe: &Path) -> String {
    format!("\"{}\" \"%1\"", exe.display())
}

/// The program a command from [`command`] runs.
pub fn exe_in_command(command: &str) -> Option<PathBuf> {
    let quoted = command.strip_prefix('"')?;
    let exe = &quoted[..quoted.find('"')?];
    (!exe.is_empty()).then(|| PathBuf::from(exe))
}

/// The changes that add the jot at `exe` to Open with and Default apps.
pub fn add(exe: &Path) -> Vec<Op> {
    let command = command(exe);
    let icon = format!("{},0", exe.display());
    let mut ops = Vec::new();

    // Open with's "Choose another app" names jot and its types.
    let app = applications_key(exe);
    ops.push(set(&app, "FriendlyAppName", APP_NAME));
    for extension in EXTENSIONS {
        ops.push(set(format!(r"{app}\SupportedTypes"), extension, ""));
    }
    ops.push(set(format!(r"{app}\shell\open\command"), "", &command));

    // Each type's Open with menu lists the ProgID.
    for extension in EXTENSIONS {
        ops.push(Op::Set {
            key: open_with_progids_key(extension),
            name: PROG_ID.into(),
            value: Value::Empty,
        });
    }

    // Default apps lists jot with its types.
    ops.push(set(CAPABILITIES, "ApplicationName", APP_NAME));
    ops.push(set(CAPABILITIES, "ApplicationDescription", DESCRIPTION));
    ops.push(set(CAPABILITIES, "ApplicationIcon", &icon));
    for extension in EXTENSIONS {
        ops.push(set(file_associations_key(), extension, PROG_ID));
    }
    ops.push(set(REGISTERED_APPLICATIONS, APP_ID, CAPABILITIES));

    // The ProgID comes last, since its command marks jot as added: one
    // stopped partway can be added again.
    let prog_id = prog_id_key();
    ops.push(set(&prog_id, "", TYPE_NAME));
    ops.push(set(&prog_id, "AppUserModelID", APP_ID));
    ops.push(set(format!(r"{prog_id}\DefaultIcon"), "", &icon));
    ops.push(set(command_key(), "", &command));
    ops
}

/// The changes that take out what [`add`] put in, for the registration in
/// `installed`. `exe` is the running jot, whose name the program key has
/// when the registry doesn't say which jot was added.
pub fn remove(installed: &Installed, exe: &Path) -> Vec<Op> {
    let mut ops = vec![Op::DeleteValue {
        key: REGISTERED_APPLICATIONS.into(),
        name: APP_ID.into(),
    }];
    for extension in extensions(installed) {
        let progids = open_with_progids_key(&extension);
        ops.push(Op::DeleteValue {
            key: progids.clone(),
            name: PROG_ID.into(),
        });
        ops.push(Op::DeleteIfEmpty { key: progids });
        ops.push(Op::DeleteIfEmpty {
            key: format!(r"{CLASSES}\{extension}"),
        });
    }
    ops.push(Op::DeleteTree {
        key: applications_key(installed.exe.as_deref().unwrap_or(exe)),
    });
    ops.push(Op::DeleteTree {
        key: CAPABILITIES.into(),
    });
    ops.push(Op::DeleteIfEmpty { key: APP.into() });
    ops.push(Op::DeleteIfEmpty { key: VENDOR.into() });
    // The ProgID goes last, so one stopped partway can be removed again.
    ops.push(Op::DeleteTree { key: prog_id_key() });
    ops
}

/// The changes that bring the registration in `installed` up to date for
/// the jot at `exe`, such as after jot.exe moved, or `None` when it is.
pub fn update(installed: &Installed, exe: &Path) -> Option<Vec<Op>> {
    let moved = !installed
        .exe
        .as_deref()
        .is_some_and(|added| same_path(added, exe));
    let types_changed = {
        let mut added: Vec<String> = installed
            .extensions
            .iter()
            .map(|extension| extension.to_lowercase())
            .collect();
        added.sort();
        let mut supported: Vec<&str> = EXTENSIONS.to_vec();
        supported.sort();
        added != supported
    };
    if !moved && !types_changed {
        return None;
    }
    let mut ops = remove(installed, exe);
    ops.extend(add(exe));
    Some(ops)
}

fn set(key: impl Into<String>, name: &str, text: &str) -> Op {
    Op::Set {
        key: key.into(),
        name: name.into(),
        value: Value::Text(text.into()),
    }
}

fn prog_id_key() -> String {
    format!(r"{CLASSES}\{PROG_ID}")
}

/// The key Windows looks a program up by, from its file name. The name is
/// taken as Windows splits paths, so that tests on other systems agree.
fn applications_key(exe: &Path) -> String {
    let exe = exe.to_string_lossy();
    let name = exe.rsplit(['\\', '/']).next().unwrap_or_default();
    let name = if name.is_empty() { "jot.exe" } else { name };
    format!(r"{CLASSES}\Applications\{name}")
}

fn open_with_progids_key(extension: &str) -> String {
    format!(r"{CLASSES}\{extension}\OpenWithProgids")
}

/// The types to take jot off: those it supports and those an earlier jot
/// added. A name that isn't an extension is skipped, so it can't lead
/// outside the type's own key.
fn extensions(installed: &Installed) -> Vec<String> {
    let mut extensions: Vec<String> = EXTENSIONS.iter().map(|ext| ext.to_string()).collect();
    for extension in &installed.extensions {
        let is_extension = extension.len() > 1
            && extension.starts_with('.')
            && !extension[1..].contains(['.', '\\', '/']);
        if is_extension
            && !extensions
                .iter()
                .any(|known| known.eq_ignore_ascii_case(extension))
        {
            extensions.push(extension.clone());
        }
    }
    extensions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exe() -> PathBuf {
        PathBuf::from(r"C:\Users\me\Programs\Jot\jot.exe")
    }

    /// The values `ops` set, as `key`, `name`, `value`.
    fn sets(ops: &[Op]) -> Vec<(String, String, Value)> {
        ops.iter()
            .filter_map(|op| match op {
                Op::Set { key, name, value } => Some((key.clone(), name.clone(), value.clone())),
                _ => None,
            })
            .collect()
    }

    fn text(value: &str) -> Value {
        Value::Text(value.into())
    }

    fn installed(exe: PathBuf) -> Installed {
        Installed {
            exe: Some(exe),
            extensions: EXTENSIONS.iter().map(|ext| ext.to_string()).collect(),
        }
    }

    #[test]
    fn add_writes_the_progid_the_program_and_default_apps() {
        let ops = add(&exe());
        assert!(ops.iter().all(|op| matches!(op, Op::Set { .. })));
        let sets = sets(&ops);
        let command = r#""C:\Users\me\Programs\Jot\jot.exe" "%1""#;
        let icon = r"C:\Users\me\Programs\Jot\jot.exe,0";
        let progid = r"Software\Classes\MesaHillsResearch.Jot.Document";
        let app = r"Software\Classes\Applications\jot.exe";
        let capabilities = r"Software\MesaHillsResearch\Jot\Capabilities";
        for expected in [
            (progid.to_string(), "", text("Text Document")),
            (
                progid.into(),
                "AppUserModelID",
                text("MesaHillsResearch.Jot"),
            ),
            (format!(r"{progid}\DefaultIcon"), "", text(icon)),
            (format!(r"{progid}\shell\open\command"), "", text(command)),
            (app.into(), "FriendlyAppName", text("Jot")),
            (format!(r"{app}\SupportedTypes"), ".md", text("")),
            (format!(r"{app}\shell\open\command"), "", text(command)),
            (
                r"Software\Classes\.txt\OpenWithProgids".into(),
                "MesaHillsResearch.Jot.Document",
                Value::Empty,
            ),
            (capabilities.into(), "ApplicationName", text("Jot")),
            (capabilities.into(), "ApplicationIcon", text(icon)),
            (
                format!(r"{capabilities}\FileAssociations"),
                ".json",
                text("MesaHillsResearch.Jot.Document"),
            ),
            (
                r"Software\RegisteredApplications".into(),
                "MesaHillsResearch.Jot",
                text(capabilities),
            ),
        ] {
            let expected = (expected.0, expected.1.to_string(), expected.2);
            assert!(sets.contains(&expected), "{expected:?}");
        }
        // Each type has its Open with entry, its supported type and its
        // association.
        for extension in EXTENSIONS {
            let keys = sets
                .iter()
                .filter(|(key, name, _)| key.contains(extension) || name == extension)
                .count();
            assert_eq!(keys, 3, "{extension}");
        }
        // The command marks jot as added, so it comes last.
        assert_eq!(
            ops.last(),
            Some(&Op::Set {
                key: command_key(),
                name: String::new(),
                value: text(command),
            })
        );
    }

    #[test]
    fn add_leaves_the_default_app_and_other_apps_alone() {
        for op in add(&exe()) {
            let Op::Set { key, name, .. } = op else {
                unreachable!()
            };
            assert!(!key.contains("UserChoice"), "{key}");
            assert!(!key.contains("Explorer"), "{key}");
            let ours = key.starts_with(r"Software\Classes\MesaHillsResearch.Jot.Document")
                || key.starts_with(r"Software\Classes\Applications\jot.exe")
                || key.starts_with(r"Software\MesaHillsResearch\Jot")
                // Shared keys, where jot only adds a value of its own.
                || (key.ends_with(r"\OpenWithProgids") && name == PROG_ID)
                || (key == r"Software\RegisteredApplications" && name == APP_ID);
            assert!(ours, "{key} {name}");
            // A type's own key, whose default value names its default app,
            // gets nothing.
            assert!(
                !EXTENSIONS
                    .iter()
                    .any(|extension| key == format!(r"Software\Classes\{extension}")),
                "{key}"
            );
        }
    }

    #[test]
    fn remove_takes_out_exactly_what_add_put_in() {
        let added = add(&exe());
        let removed = remove(&installed(exe()), &exe());

        // Every value add set in a shared key is deleted by name, and every
        // key of jot's own is deleted with all in it.
        let deletes_value = |key: &str, name: &str| {
            removed.contains(&Op::DeleteValue {
                key: key.into(),
                name: name.into(),
            })
        };
        let deletes_tree = |key: &str| {
            removed.iter().any(|op| match op {
                Op::DeleteTree { key: tree } => {
                    key == tree || key.starts_with(&format!(r"{tree}\"))
                }
                _ => false,
            })
        };
        for (key, name, _) in sets(&added) {
            assert!(
                deletes_tree(&key) || deletes_value(&key, &name),
                "{key} {name}"
            );
        }
        // Shared keys go only when empty: another app's value keeps them.
        for op in &removed {
            match op {
                Op::DeleteTree { key } => assert!(
                    key.starts_with(r"Software\Classes\MesaHillsResearch.Jot.Document")
                        || key == r"Software\Classes\Applications\jot.exe"
                        || key == r"Software\MesaHillsResearch\Jot\Capabilities",
                    "{key}"
                ),
                Op::DeleteValue { name, .. } => assert!(name == PROG_ID || name == APP_ID),
                Op::DeleteIfEmpty { .. } => {}
                Op::Set { .. } => panic!("remove sets {op:?}"),
            }
        }
        assert!(removed.contains(&Op::DeleteIfEmpty {
            key: r"Software\Classes\.txt\OpenWithProgids".into()
        }));
        assert!(removed.contains(&Op::DeleteIfEmpty {
            key: r"Software\Classes\.txt".into()
        }));
        // The ProgID, which marks jot as added, goes last.
        assert_eq!(
            removed.last(),
            Some(&Op::DeleteTree {
                key: r"Software\Classes\MesaHillsResearch.Jot.Document".into()
            })
        );
    }

    #[test]
    fn remove_takes_out_types_an_earlier_jot_added() {
        let mut earlier = installed(exe());
        earlier.extensions.push(".text".into());
        // Names that aren't extensions don't lead to other keys.
        earlier.extensions.push(r".x\..\Software".into());
        earlier.extensions.push("".into());
        let removed = remove(&earlier, &exe());
        assert!(removed.contains(&Op::DeleteValue {
            key: r"Software\Classes\.text\OpenWithProgids".into(),
            name: PROG_ID.into(),
        }));
        for op in &removed {
            let (Op::DeleteValue { key, .. } | Op::DeleteTree { key } | Op::DeleteIfEmpty { key }) =
                op
            else {
                panic!("remove sets {op:?}");
            };
            assert!(!key.contains(r".x\") && !key.contains(r"\\"), "{key}");
        }
        let progids = removed
            .iter()
            .filter(|op| matches!(op, Op::DeleteValue { name, .. } if name == PROG_ID))
            .count();
        assert_eq!(progids, EXTENSIONS.len() + 1);
    }

    #[test]
    fn the_program_is_named_by_its_file_name() {
        let renamed = PathBuf::from(r"D:\Tools\jot-nightly.exe");
        let sets = sets(&add(&renamed));
        assert!(sets.iter().any(|(key, name, _)| {
            key == r"Software\Classes\Applications\jot-nightly.exe" && name == "FriendlyAppName"
        }));
        // Removing it takes out the key of the jot that was added.
        let removed = remove(&installed(renamed), &exe());
        assert!(removed.contains(&Op::DeleteTree {
            key: r"Software\Classes\Applications\jot-nightly.exe".into()
        }));
        assert!(!removed.contains(&Op::DeleteTree {
            key: r"Software\Classes\Applications\jot.exe".into()
        }));
    }

    #[test]
    fn the_command_names_the_program() {
        let command = command(&exe());
        assert_eq!(exe_in_command(&command), Some(exe()));
        assert_eq!(
            exe_in_command(r#""D:\a b\jot.exe" --flag "%1""#),
            Some(PathBuf::from(r"D:\a b\jot.exe"))
        );
        assert_eq!(exe_in_command(r"C:\jot.exe %1"), None);
        assert_eq!(exe_in_command(r#""" "%1""#), None);
        assert_eq!(exe_in_command(r#""C:\jot.exe"#), None);
    }

    #[test]
    fn a_registration_for_this_jot_needs_no_update() {
        assert_eq!(update(&installed(exe()), &exe()), None);
        // Windows compares file names without regard to case.
        let shouted = PathBuf::from(r"C:\USERS\ME\Programs\Jot\JOT.EXE");
        assert_eq!(update(&installed(shouted), &exe()), None);
    }

    #[test]
    fn a_moved_jot_updates_the_registration() {
        let moved = PathBuf::from(r"D:\Apps\Jot\jot.exe");
        let ops = update(&installed(exe()), &moved).unwrap();
        let mut expected = remove(&installed(exe()), &moved);
        expected.extend(add(&moved));
        assert_eq!(ops, expected);
        assert_eq!(
            ops.last(),
            Some(&Op::Set {
                key: command_key(),
                name: String::new(),
                value: text(r#""D:\Apps\Jot\jot.exe" "%1""#),
            })
        );

        // So does a command jot can't read, or types that changed.
        let unreadable = Installed {
            exe: None,
            ..installed(exe())
        };
        assert!(update(&unreadable, &exe()).is_some());
        let fewer = Installed {
            extensions: vec![".txt".into()],
            ..installed(exe())
        };
        assert!(update(&fewer, &exe()).is_some());
    }
}
