//! What `siftr …` means when it doesn't start with a subcommand, decided before clap parses. Never a guess:
//!
//! 1. what follows `--` is a command to wrap (`siftr -- make` is `siftr run -- make`);
//! 2. a subcommand;
//! 3. a preset (`cron`);
//! 4. an existing file, or `-`, is ingested (`./cron` for a file named like a preset);
//! 5. nothing at all, with stdin piped or redirected: ingest stdin;
//! 6. nothing at all otherwise: clap prints help;
//! 7. any other word is an error naming the subcommands and presets, never input.

use std::ffi::OsString;
use std::path::Path;

use crate::output;

pub const SUBCOMMANDS: &[&str] = &[
    "run", "ingest", "follow", "changes", "summary", "explain", "ack", "history", "sources",
    "status", "gc",
];
pub const PRESETS: &[&str] = &["cron"];

pub enum PathKind {
    File,
    Dir,
}

/// What dispatch needs to know about the machine, injected so every row is testable without one.
pub struct Env<'a> {
    pub stdin_piped: bool,
    pub path_kind: &'a dyn Fn(&Path) -> Option<PathKind>,
    /// The context a file ingested by path compares within.
    pub context_for: &'a dyn Fn(&Path) -> String,
}

/// `args` without the program name, rewritten into an explicit subcommand, or why they can't be.
pub fn dispatch(mut args: Vec<OsString>, env: &Env) -> anyhow::Result<Vec<OsString>> {
    let at = globals_end(&args);
    let Some(first) = args.get(at).map(|arg| arg.to_string_lossy().into_owned()) else {
        if env.stdin_piped {
            args.push("ingest".into());
        }
        return Ok(args);
    };
    let wraps = args[at..]
        .iter()
        .position(|arg| arg == "--")
        .is_some_and(|end| args[at..at + end].iter().all(is_flag));
    if wraps {
        args.insert(at, "run".into());
        return Ok(args);
    }
    if is_flag(&args[at])
        || first == "help"
        || SUBCOMMANDS.contains(&first.as_str())
        || PRESETS.contains(&first.as_str())
    {
        return Ok(args);
    }
    if first == "-" {
        args[at] = "ingest".into();
        return Ok(args);
    }
    let path = Path::new(&args[at]);
    match (env.path_kind)(path) {
        Some(PathKind::File) => {
            // A dated or rotated file names its own context: `siftr app-0915.log --context app`.
            let named = args[at..]
                .iter()
                .filter_map(|arg| arg.to_str())
                .any(|arg| arg == "--context" || arg.starts_with("--context="));
            let mut ingest: Vec<OsString> = vec!["ingest".into()];
            if !named {
                ingest.extend(["--context".into(), (env.context_for)(path).into()]);
            }
            args.splice(at..at, ingest);
            Ok(args)
        }
        Some(PathKind::Dir) => Err(output::usage(format!(
            "{first} is a directory; siftr ingest --dir {first} replays a captured scenario"
        ))),
        None if first.contains('/') => Err(output::not_found(format!("no such file: {first}"))),
        None => Err(output::usage(unknown(&first))),
    }
}

/// Where the global flags before a subcommand end: they may precede any row.
fn globals_end(args: &[OsString]) -> usize {
    let mut at = 0;
    while let Some(arg) = args.get(at).and_then(|arg| arg.to_str()) {
        at += match arg {
            "--home" => 2,
            "-j" | "--json" => 1,
            _ if arg.starts_with("--home=") => 1,
            _ => break,
        };
    }
    at.min(args.len())
}

fn is_flag(arg: &OsString) -> bool {
    arg.to_str()
        .is_some_and(|arg| arg.len() > 1 && arg.starts_with('-') && arg != "--")
}

/// Commands that were removed, and the exact thing to type instead. A word a user's fingers still
/// type deserves the answer rather than the whole list — and the edit distance below cannot reach
/// either of these: `dismiss` is nowhere near `ack`, nor `evidence` near `explain`.
const RETIRED: &[(&str, &str)] = &[
    ("dismiss", "siftr ack SIGNAL --wrong"),
    ("evidence", "siftr explain BEHAVIOR"),
];

fn unknown(word: &str) -> String {
    if let Some((_, replacement)) = RETIRED.iter().find(|(name, _)| *name == word) {
        return format!("'{word}' was removed; use `{replacement}`");
    }
    let nearest = SUBCOMMANDS
        .iter()
        .chain(PRESETS)
        .map(|name| (distance(word, name), *name))
        .filter(|&(d, name)| d <= 2 && d < name.len())
        .min();
    let hint = nearest.map_or(String::new(), |(_, name)| {
        format!("did you mean '{name}'? ")
    });
    format!(
        "'{word}' is not a command, preset or existing file; {hint}commands: {}; presets: {}",
        SUBCOMMANDS.join(", "),
        PRESETS.join(", ")
    )
}

/// Levenshtein distance, for "did you mean".
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let substituted = diagonal + usize::from(ca != *cb);
            diagonal = row[j + 1];
            row[j + 1] = substituted.min(row[j] + 1).min(diagonal + 1);
        }
    }
    row[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(files: &[&str], dirs: &[&str], piped: bool, args: &[&str]) -> Result<String, String> {
        let path_kind = |path: &Path| {
            let path = path.to_str().unwrap();
            if files.contains(&path) {
                Some(PathKind::File)
            } else if dirs.contains(&path) {
                Some(PathKind::Dir)
            } else {
                None
            }
        };
        let context_for = |path: &Path| format!("ctx:{}", path.display());
        let env = Env {
            stdin_piped: piped,
            path_kind: &path_kind,
            context_for: &context_for,
        };
        dispatch(args.iter().map(OsString::from).collect(), &env)
            .map(|args| {
                args.iter()
                    .map(|a| a.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .map_err(|error| format!("{error:#}"))
    }

    fn ok(args: &[&str]) -> String {
        with(
            &["log/production.log", "./cron", "./status"],
            &["fixtures"],
            false,
            args,
        )
        .unwrap()
    }

    #[test]
    fn a_wrapped_command_follows_the_double_dash() {
        assert_eq!(ok(&["--", "backup.sh", "-v"]), "run -- backup.sh -v");
        assert_eq!(ok(&["-q", "--", "rspec"]), "run -q -- rspec");
        assert_eq!(
            ok(&["-j", "--home", "h", "--", "rspec"]),
            "-j --home h run -- rspec"
        );
        assert_eq!(
            ok(&["run", "--", "rspec"]),
            "run -- rspec",
            "the explicit spelling stays"
        );
        // What follows `--` is the command's, even when it names a subcommand or a file.
        assert_eq!(ok(&["--", "status"]), "run -- status");
    }

    #[test]
    fn subcommands_and_presets_win_over_files_of_the_same_name() {
        let files = ["status", "cron", "history"];
        for word in files {
            assert_eq!(with(&files, &[], true, &[word]).unwrap(), word);
        }
        assert_eq!(ok(&["--home=h", "status", "-j"]), "--home=h status -j");
        assert_eq!(
            ok(&["./cron"]),
            "ingest --context ctx:./cron ./cron",
            "./ picks the file"
        );
        assert_eq!(ok(&["./status"]), "ingest --context ctx:./status ./status");
    }

    #[test]
    fn an_existing_file_or_dash_is_ingested() {
        assert_eq!(
            ok(&["-j", "log/production.log"]),
            "-j ingest --context ctx:log/production.log log/production.log"
        );
        assert_eq!(
            ok(&["log/production.log", "--context", "app"]),
            "ingest log/production.log --context app",
            "a context given is kept"
        );
        assert_eq!(ok(&["-"]), "ingest");
        let dir = with(&[], &["fixtures"], false, &["fixtures"]).unwrap_err();
        assert!(dir.contains("siftr ingest --dir fixtures"), "{dir}");
        let missing = with(&[], &[], false, &["log/missing.log"]).unwrap_err();
        assert_eq!(missing, "no such file: log/missing.log");
    }

    #[test]
    fn no_arguments_read_piped_stdin_or_leave_clap_to_print_help() {
        assert_eq!(with(&[], &[], true, &[]).unwrap(), "ingest");
        assert_eq!(with(&[], &[], true, &["-j"]).unwrap(), "-j ingest");
        assert_eq!(with(&[], &[], false, &[]).unwrap(), "");
        assert_eq!(ok(&["--help"]), "--help");
    }

    #[test]
    fn an_unknown_word_is_an_error_with_suggestions_never_input() {
        let typo = with(&[], &[], true, &["statu"]).unwrap_err();
        assert!(
            typo.starts_with(
                "'statu' is not a command, preset or existing file; did you mean 'status'?"
            ),
            "{typo}"
        );
        assert!(typo.contains("presets: cron"), "{typo}");
        assert!(
            with(&[], &[], false, &["crn"])
                .unwrap_err()
                .contains("did you mean 'cron'?")
        );
        let far = with(&[], &[], false, &["xyzzy"]).unwrap_err();
        assert!(!far.contains("did you mean"), "{far}");
    }

    /// A command we removed is a word fingers keep typing, and it is too far from its replacement for
    /// the edit distance to find: answer it instead of handing back the whole list.
    #[test]
    fn a_retired_command_names_what_replaced_it() {
        let gone = with(&[], &[], false, &["dismiss"]).unwrap_err();
        assert_eq!(
            gone,
            "'dismiss' was removed; use `siftr ack SIGNAL --wrong`"
        );
        assert!(
            distance("dismiss", "ack") > 2,
            "the fallback could have found it"
        );
    }
}
