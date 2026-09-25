//! What `siftr …` means when it doesn't start with a subcommand, decided before clap parses. Never a guess:
//!
//! 1. what follows `--` is a command to wrap (`siftr -- make` is `siftr run -- make`);
//! 2. a subcommand;
//! 3. a preset (`cron`);
//! 4. an existing file, or `-`, is read (`./cron` for a file named like a preset), and a directory
//!    holding a captured scenario is replayed;
//! 5. nothing but flags, with stdin piped or redirected: read stdin (`tail -f log | siftr -J`),
//!    except `--help`/`--version`, which answer for themselves;
//! 6. nothing at all otherwise: clap prints help;
//! 7. any other word is an error naming the subcommands and presets — or, when it is a program on
//!    PATH, the line that wraps it — never input.

use std::ffi::OsString;
use std::path::Path;

use crate::output;

pub const SUBCOMMANDS: &[&str] = &[
    "run", "changes", "summary", "explain", "ack", "history", "sources", "status", "gc",
];
pub const PRESETS: &[&str] = &["cron"];

pub enum PathKind {
    File,
    /// A directory holding a captured scenario: at least one of the files a replay reads.
    Scenario,
    Dir,
}

/// What dispatch needs to know about the machine, injected so every row is testable without one.
pub struct Env<'a> {
    pub stdin_piped: bool,
    pub path_kind: &'a dyn Fn(&Path) -> Option<PathKind>,
    /// The context a file read by path compares within.
    pub context_for: &'a dyn Fn(&Path) -> String,
    /// Whether a word names a program a shell would find, for the error that says how to wrap it.
    pub on_path: &'a dyn Fn(&str) -> bool,
}

/// `args` without the program name, rewritten into an explicit subcommand, or why they can't be.
pub fn dispatch(mut args: Vec<OsString>, env: &Env) -> anyhow::Result<Vec<OsString>> {
    let at = globals_end(&args);
    if args.get(at).is_none() {
        if env.stdin_piped {
            args.push("ingest".into());
        }
        return Ok(args);
    }
    let wraps = args[at..]
        .iter()
        .position(|arg| arg == "--")
        .is_some_and(|end| args[at..at + end].iter().all(is_flag));
    if wraps {
        args.insert(at, "run".into());
        return Ok(args);
    }
    // Flags and nothing to apply them to: piped stdin is the input, so this reads it, and
    // `tail -f log | siftr -J` streams rather than printing help. `--help` and `--version` are answers
    // in themselves, and a positional we didn't recognise stays an error rather than becoming a flag's
    // argument.
    if args[at..].iter().all(is_flag) && env.stdin_piped && !args[at..].iter().any(is_answer) {
        args.insert(at, "ingest".into());
        return Ok(args);
    }
    // A read's input may precede or follow the read's own valueless flags: `siftr -J app.log` and
    // `siftr app.log -J` are one command, so normalize to the second and let the one ladder below decide
    // what the input is. Only valueless flags move — `--context app.log` would otherwise have its value
    // taken for the input — and only when something follows them, so a bare flag stays clap's to answer.
    let lead = args[at..].iter().take_while(|arg| valueless(arg)).count();
    if lead > 0 && at + lead < args.len() {
        args[at..].rotate_left(lead);
    }
    let first = args[at].to_string_lossy().into_owned();
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
        // A replay gets no synthesized context: the scenarios of one sequence are meant to compare with each
        // other, and a context per directory would leave every one of them with nothing to compare against.
        Some(PathKind::Scenario) => {
            args.splice(at..at, ["ingest".into(), "--dir".into()]);
            Ok(args)
        }
        Some(PathKind::Dir) => Err(output::usage(format!(
            "{first} is a directory, and not a captured scenario: one holds at least one of {}",
            crate::cmd::ingest::scenario_files().join(", ")
        ))),
        None if first.contains('/') => Err(output::not_found(format!("no such file: {first}"))),
        None => Err(output::usage(unknown(&first, &args[at + 1..], env))),
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

/// A flag a read takes that consumes no argument, and so may sit either side of the input. `--home` and `-j`
/// are already behind us (`globals_end`); `--dir` and `--context` take values and stay where they were typed.
fn valueless(arg: &OsString) -> bool {
    matches!(
        arg.to_str(),
        Some("-J" | "--ndjson" | "--quiet-unless-changed" | "--no-report")
    )
}

/// A flag that is the whole answer, so it never becomes a subcommand's.
fn is_answer(arg: &OsString) -> bool {
    matches!(arg.to_str(), Some("-h" | "--help" | "-V" | "--version"))
}

/// Commands that were removed, the exact thing to type instead, and — where the replacement does more than
/// the word did — what the reader would otherwise assume they had lost. A word a user's fingers still type
/// deserves the answer rather than the whole list, and the edit distance below cannot reach any of these:
/// `dismiss` is nowhere near `ack`, nor `evidence` near `explain`, nor `follow` or `ingest` near anything.
const RETIRED: &[(&str, &str, &str)] = &[
    ("dismiss", "siftr ack SIGNAL --wrong", ""),
    ("evidence", "siftr explain BEHAVIOR", ""),
    (
        "follow",
        "siftr - (or siftr FILE)",
        "it streams the same behaviors, and then records and compares the run",
    ),
    (
        "ingest",
        "siftr FILE (or siftr -, siftr DIR)",
        "a captured scenario replays by naming its directory, as --dir did",
    ),
];

/// `word` isn't a command: say what it is instead of handing back the list, whenever siftr can tell. `rest` is
/// what followed it, so a program on PATH gets a line to paste rather than a form to adapt.
fn unknown(word: &str, rest: &[OsString], env: &Env) -> String {
    if let Some((_, replacement, why)) = RETIRED.iter().find(|(name, ..)| *name == word) {
        let why = match *why {
            "" => String::new(),
            why => format!(" — {why}"),
        };
        return format!("'{word}' was removed; use `{replacement}`{why}");
    }
    // siftr reserves its first word, as `git` and `cargo` do, so a program never runs by happening to be
    // installed — the same command line would then mean different things on different machines. The
    // ergonomics go in the error instead, ahead of "did you mean" because `gcc` is not a misspelt `gc`.
    if (env.on_path)(word) {
        let argv: Vec<String> = std::iter::once(word.to_owned())
            .chain(rest.iter().map(|arg| arg.to_string_lossy().into_owned()))
            .collect();
        return format!(
            "'{word}' is not a command, preset or existing file; to run it: siftr -- {}",
            siftr::context::shell_join(&argv)
        );
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

    /// A directory whose name starts with this is a captured scenario; any other is a plain directory.
    const SCENARIO: &str = "scenario";
    /// The one program these tests pretend is installed: what is really on the box must never decide a row.
    const INSTALLED: &str = "rspec";

    fn with(files: &[&str], dirs: &[&str], piped: bool, args: &[&str]) -> Result<String, String> {
        let path_kind = |path: &Path| {
            let path = path.to_str().unwrap();
            if files.contains(&path) {
                Some(PathKind::File)
            } else if !dirs.contains(&path) {
                None
            } else if path.starts_with(SCENARIO) {
                Some(PathKind::Scenario)
            } else {
                Some(PathKind::Dir)
            }
        };
        let context_for = |path: &Path| format!("ctx:{}", path.display());
        let on_path = |word: &str| word == INSTALLED;
        let env = Env {
            stdin_piped: piped,
            path_kind: &path_kind,
            context_for: &context_for,
            on_path: &on_path,
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
            &["fixtures", "scenario/baseline"],
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
    fn an_existing_file_or_dash_is_read() {
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
        let missing = with(&[], &[], false, &["log/missing.log"]).unwrap_err();
        assert_eq!(missing, "no such file: log/missing.log");
    }

    /// A directory is replayed when it says it is a capture, and refused when it doesn't: recognition, not a
    /// guess. Unlike a file it gets no context of its own — the scenarios of one sequence compare with each
    /// other, and a context per directory would leave each of them nothing to compare against.
    #[test]
    fn a_directory_replays_only_when_it_holds_a_captured_scenario() {
        assert_eq!(ok(&["scenario/baseline"]), "ingest --dir scenario/baseline");
        assert_eq!(
            ok(&["-j", "scenario/baseline", "--context", "demo"]),
            "-j ingest --dir scenario/baseline --context demo"
        );
        let plain = with(&[], &["fixtures"], false, &["fixtures"]).unwrap_err();
        assert!(
            plain.starts_with("fixtures is a directory, and not a captured scenario: one holds"),
            "{plain}"
        );
        assert!(
            plain.contains("rspec.ndjson"),
            "it names the files: {plain}"
        );
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

    /// A program still isn't a subcommand — running one because it happens to be installed would make the same
    /// command line mean different things on different machines. But the error can hand back the line that
    /// does run it, arguments and all, so it is a paste rather than a form to fill in.
    #[test]
    fn a_program_on_path_is_told_how_to_be_wrapped_not_run() {
        let wrap = with(
            &[],
            &[],
            false,
            &[INSTALLED, "spec/a_spec.rb", "-e", "n + 1"],
        )
        .unwrap_err();
        assert_eq!(
            wrap,
            "'rspec' is not a command, preset or existing file; to run it: siftr -- rspec spec/a_spec.rb -e 'n + 1'"
        );
        // Not installed: still the list, and still the near match.
        let absent = with(&[], &[], false, &["statu"]).unwrap_err();
        assert!(absent.contains("did you mean 'status'?"), "{absent}");
    }

    /// `-J` is ingest's flag, not a global, so a lone one used to reach clap as a subcommandless
    /// invocation and print help — with a log on stdin waiting to be read.
    #[test]
    fn flags_over_a_piped_stdin_ingest_it() {
        assert_eq!(with(&[], &[], true, &["-J"]).unwrap(), "ingest -J");
        assert_eq!(
            with(&[], &[], true, &["--no-report"]).unwrap(),
            "ingest --no-report"
        );
        assert_eq!(
            with(&[], &[], true, &["-j", "-J"]).unwrap(),
            "-j ingest -J",
            "a global still lifts ahead of the subcommand"
        );
        for answer in ["-h", "--help", "-V", "--version"] {
            assert_eq!(
                with(&[], &[], true, &[answer]).unwrap(),
                answer,
                "{answer} answers for itself"
            );
        }
        assert_eq!(
            with(&[], &[], false, &["-J"]).unwrap(),
            "-J",
            "no pipe, no input: still clap's to reject"
        );
        assert_eq!(
            with(&[], &[], true, &["-J", "nope"]).unwrap_err(),
            "'nope' is not a command, preset or existing file; commands: run, changes, summary, explain, \
             ack, history, sources, status, gc; presets: cron",
            "an unrecognised positional never becomes a flag's argument: it is the error, not the flag"
        );
    }

    /// `siftr -J app.log` and `siftr app.log -J` are the same command, and only the second used to work.
    #[test]
    fn a_reads_valueless_flags_may_precede_its_input() {
        assert_eq!(
            with(&["app.log"], &[], false, &["-J", "app.log"]).unwrap(),
            "ingest --context ctx:app.log app.log -J",
            "and the input still names its own context"
        );
        assert_eq!(
            with(&["app.log"], &[], false, &["--no-report", "-J", "app.log"]).unwrap(),
            "ingest --context ctx:app.log app.log --no-report -J"
        );
        assert_eq!(
            with(&[], &[SCENARIO], false, &["-J", SCENARIO]).unwrap(),
            format!("ingest --dir {SCENARIO} -J"),
            "a replay too, and it still gets no context of its own"
        );
        assert_eq!(
            with(
                &["app.log"],
                &[],
                false,
                &["--context", "app.log", "app.log"]
            )
            .unwrap(),
            "--context app.log app.log",
            "a flag that takes a value is never stepped over, so its value cannot be read as the input; \
             this stays clap's usage error, as it was"
        );
        assert_eq!(
            with(&[], &[], false, &["-J"]).unwrap(),
            "-J",
            "a flag with nothing after it is still clap's to answer"
        );
    }

    /// A command we removed is a word fingers keep typing, and it is too far from its replacement for
    /// the edit distance to find: answer it instead of handing back the whole list.
    #[test]
    fn a_retired_command_names_what_replaced_it() {
        for (word, replacement, _) in RETIRED {
            let gone = with(&[], &[], false, &[word]).unwrap_err();
            assert!(
                gone.starts_with(&format!("'{word}' was removed; use `{replacement}`")),
                "{gone}"
            );
            // Each is beyond the did-you-mean fallback, which is the only reason this table exists.
            assert!(
                SUBCOMMANDS
                    .iter()
                    .chain(PRESETS)
                    .all(|name| distance(word, name) > 2)
            );
        }
        // `follow` did less than what replaced it, so the message has to say so or it reads as a loss.
        let follow = with(&[], &[], false, &["follow"]).unwrap_err();
        assert!(follow.contains("records and compares the run"), "{follow}");
    }
}
