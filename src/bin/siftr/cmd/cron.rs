//! `siftr cron`: what runs on a schedule here, where its output goes, and the line that would record each cron job
//! with `siftr --`. Read-only: it never edits a crontab or a launchd agent, never runs a job, and records nothing.

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use anyhow::Result;
use serde_json::{Value, json};
use siftr::context::shell_join;

use super::{Globals, found};
use crate::output;

#[derive(clap::Args)]
pub struct Args {}

/// How far back the macOS unified log is searched for cron's own lines.
const LOG_WINDOW: &str = "7d";

pub fn run(_args: Args, globals: &Globals) -> Result<ExitCode> {
    let discovery = Discovery::of(&Places::this_machine());
    output::emit(globals.json, || discovery.json(), |w| discovery.human(w))?;
    Ok(found(discovery.any()))
}

/// Where to look: this machine's places, or a test's.
pub struct Places {
    pub crontab: Vec<OsString>,
    pub system_crontab: PathBuf,
    pub cron_d: PathBuf,
    pub launch_agents: Option<PathBuf>,
    pub mail_spool: Option<PathBuf>,
    pub cron_logs: Vec<PathBuf>,
    pub unified_log: Option<Vec<OsString>>,
    /// How a wrapped line names siftr: cron's PATH is minimal, so an absolute path.
    pub siftr: String,
}

impl Places {
    fn this_machine() -> Self {
        let macos = cfg!(target_os = "macos");
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let user = std::env::var_os("USER").or_else(|| std::env::var_os("LOGNAME"));
        let predicate = r#"process == "cron""#;
        Places {
            crontab: vec!["crontab".into(), "-l".into()],
            system_crontab: "/etc/crontab".into(),
            cron_d: "/etc/cron.d".into(),
            launch_agents: home
                .filter(|_| macos)
                .map(|h| h.join("Library/LaunchAgents")),
            mail_spool: user.map(|user| Path::new("/var/mail").join(user)),
            cron_logs: vec!["/var/log/cron".into(), "/var/log/syslog".into()],
            unified_log: macos.then(|| {
                let args = [
                    "/usr/bin/log",
                    "show",
                    "--style",
                    "compact",
                    "--last",
                    LOG_WINDOW,
                    "--predicate",
                    predicate,
                ];
                args.map(OsString::from).to_vec()
            }),
            siftr: std::env::current_exe()
                .map(|exe| exe.to_string_lossy().into_owned())
                .unwrap_or_else(|_| "siftr".to_owned()),
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum Status {
    Found,
    /// There, but holding nothing on a schedule; says why.
    Empty(String),
    Absent,
    Unreadable(String),
}

impl Status {
    fn as_str(&self) -> &'static str {
        match self {
            Status::Found => "found",
            Status::Empty(_) => "empty",
            Status::Absent => "absent",
            Status::Unreadable(_) => "unreadable",
        }
    }

    fn detail(&self) -> Option<&str> {
        match self {
            Status::Empty(why) | Status::Unreadable(why) => Some(why),
            Status::Found | Status::Absent => None,
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct Job {
    pub schedule: String,
    pub user: Option<String>,
    pub command: String,
    /// The same entry recording each run through siftr; none for a launchd agent, which only its plist can change.
    pub wrapped: Option<String>,
}

pub struct Jobs {
    pub source: String,
    pub status: Status,
    pub jobs: Vec<Job>,
}

pub struct Evidence {
    pub source: String,
    pub status: Status,
    pub lines: u64,
}

pub struct Discovery {
    pub jobs: Vec<Jobs>,
    pub evidence: Vec<Evidence>,
}

impl Discovery {
    pub fn of(places: &Places) -> Self {
        let mut jobs = vec![
            user_crontab(places),
            table(
                "/etc/crontab",
                read_lines(&places.system_crontab).map(|lines| parse(&lines, true, &places.siftr)),
            ),
            cron_d(places),
        ];
        jobs.extend(places.launch_agents.as_deref().map(launch_agents));
        let mut evidence = Vec::new();
        if let Some(spool) = &places.mail_spool {
            let messages = count_lines(spool, |line| line.starts_with("Subject: Cron <"));
            evidence.push(counted(
                spool.display().to_string(),
                messages,
                "no mail from cron",
            ));
        }
        for log in &places.cron_logs {
            let lines = count_lines(log, |line| {
                ["CRON[", "cron[", "crond["]
                    .iter()
                    .any(|tag| line.contains(tag))
            });
            evidence.push(counted(
                log.display().to_string(),
                lines,
                "no lines from cron",
            ));
        }
        if let Some(command) = &places.unified_log {
            let source = format!("unified log, last {LOG_WINDOW}");
            evidence.push(counted(source, unified_log(command), "no lines from cron"));
        }
        Discovery { jobs, evidence }
    }

    fn any(&self) -> bool {
        self.jobs.iter().any(|s| !s.jobs.is_empty()) || self.evidence.iter().any(|e| e.lines > 0)
    }

    fn json(&self) -> Value {
        json!({
            "jobs": self.jobs.iter().map(|s| json!({
                "source": s.source,
                "status": s.status.as_str(),
                "detail": s.status.detail(),
                "entries": s.jobs.iter().map(|j| json!({
                    "schedule": j.schedule,
                    "user": j.user,
                    "command": j.command,
                    "wrapped": j.wrapped,
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "evidence": self.evidence.iter().map(|e| json!({
                "source": e.source,
                "status": e.status.as_str(),
                "detail": e.status.detail(),
                "lines": e.lines,
            })).collect::<Vec<_>>(),
        })
    }

    fn human(&self, w: &mut dyn Write) -> io::Result<()> {
        writeln!(w, "scheduled jobs")?;
        for source in &self.jobs {
            let found = output::plural(source.jobs.len() as u64, "job");
            writeln!(
                w,
                "  {:<24} {}",
                source.source,
                describe(&source.status, &found)
            )?;
            for job in &source.jobs {
                let user = job
                    .user
                    .as_deref()
                    .map_or(String::new(), |u| format!("{u}  "));
                writeln!(w, "    {}  {user}{}", job.schedule, job.command)?;
                if let Some(wrapped) = &job.wrapped {
                    writeln!(w, "      record it: {wrapped}")?;
                }
            }
        }
        writeln!(w, "where cron's output goes")?;
        for e in &self.evidence {
            let found = output::plural(e.lines, "line");
            writeln!(w, "  {:<24} {}", e.source, describe(&e.status, &found))?;
        }
        let wrappable = self
            .jobs
            .iter()
            .flat_map(|s| &s.jobs)
            .any(|j| j.wrapped.is_some());
        if wrappable {
            writeln!(
                w,
                "note: a wrapped job adds nothing to cron's mail unless something changed or is still open"
            )?;
            writeln!(
                w,
                "next: crontab -e, and replace a job with its record-it line"
            )
        } else {
            writeln!(
                w,
                "next: siftr -- CMD records a command's runs and compares them"
            )
        }
    }
}

fn describe(status: &Status, found: &str) -> String {
    match status {
        Status::Found => found.to_owned(),
        Status::Empty(why) => why.clone(),
        Status::Absent => "absent".to_owned(),
        Status::Unreadable(why) => format!("unreadable: {why}"),
    }
}

fn user_crontab(places: &Places) -> Jobs {
    let source = places
        .crontab
        .iter()
        .map(|a| a.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    let (program, args) = places.crontab.split_first().expect("a crontab command");
    let (status, jobs) = match Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
    {
        Err(error) => (unreadable(&error), Vec::new()),
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            let jobs = parse(&text.lines().collect::<Vec<_>>(), false, &places.siftr);
            (found_or(&jobs, "no jobs in the crontab"), jobs)
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let first = stderr.lines().next().unwrap_or("failed").trim();
            // Vixie cron and cronie both say "no crontab for USER", exiting 1.
            let status = match first.contains("no crontab") {
                true => Status::Empty("no crontab for this user".to_owned()),
                false => Status::Unreadable(first.to_owned()),
            };
            (status, Vec::new())
        }
    };
    Jobs {
        source,
        status,
        jobs,
    }
}

fn found_or(jobs: &[Job], empty: &str) -> Status {
    if jobs.is_empty() {
        Status::Empty(empty.to_owned())
    } else {
        Status::Found
    }
}

fn table(source: &str, read: io::Result<Vec<Job>>) -> Jobs {
    let (status, jobs) = match read {
        Ok(jobs) => (found_or(&jobs, "no jobs"), jobs),
        Err(error) => (unreadable(&error), Vec::new()),
    };
    Jobs {
        source: source.to_owned(),
        status,
        jobs,
    }
}

fn cron_d(places: &Places) -> Jobs {
    let entries = match std::fs::read_dir(&places.cron_d) {
        Ok(entries) => entries,
        Err(error) => return table("/etc/cron.d", Err(error)),
    };
    let mut files: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    files.sort();
    let mut jobs = Vec::new();
    for file in files {
        // One unreadable file hides only its own jobs.
        if let Ok(lines) = read_lines(&file) {
            jobs.extend(parse(&lines, true, &places.siftr));
        }
    }
    table("/etc/cron.d", Ok(jobs))
}

/// User agents launchd starts on a calendar or an interval. The key is plain bytes in XML and binary plists alike.
fn launch_agents(dir: &Path) -> Jobs {
    let source = "~/Library/LaunchAgents";
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => return table(source, Err(error)),
    };
    let mut plists: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "plist"))
        .collect();
    plists.sort();
    let jobs = plists
        .iter()
        .filter_map(|plist| {
            let bytes = std::fs::read(plist).ok()?;
            let key = ["StartCalendarInterval", "StartInterval"]
                .into_iter()
                .find(|key| bytes.windows(key.len()).any(|w| w == key.as_bytes()))?;
            Some(Job {
                schedule: key.to_owned(),
                user: None,
                command: plist.file_stem()?.to_string_lossy().into_owned(),
                wrapped: None,
            })
        })
        .collect::<Vec<_>>();
    Jobs {
        source: source.to_owned(),
        status: found_or(&jobs, "no agent on a schedule"),
        jobs,
    }
}

/// Crontab entries: `m h dom mon dow [user] command`, or `@daily [user] command`. Comments and `NAME=value`
/// environment lines aren't jobs.
pub fn parse(lines: &[impl AsRef<str>], with_user: bool, siftr: &str) -> Vec<Job> {
    lines
        .iter()
        .filter_map(|line| {
            let line = line.as_ref().trim();
            let first = line.split_whitespace().next()?;
            if line.starts_with('#') || (first.contains('=') && !first.starts_with('@')) {
                return None;
            }
            let fields = if first.starts_with('@') { 1 } else { 5 };
            let mut rest = line;
            let mut schedule = Vec::new();
            for _ in 0..fields {
                let (field, tail) = split_field(rest)?;
                schedule.push(field);
                rest = tail;
            }
            let user = match with_user {
                true => {
                    let (user, tail) = split_field(rest)?;
                    rest = tail;
                    Some(user.to_owned())
                }
                false => None,
            };
            let command = rest.trim();
            if command.is_empty() {
                return None;
            }
            let mut job = Job {
                schedule: schedule.join(" "),
                user,
                command: command.to_owned(),
                wrapped: None,
            };
            job.wrapped = Some(wrap(&job, siftr));
            Some(job)
        })
        .collect()
}

fn split_field(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start();
    let end = s.find(char::is_whitespace).unwrap_or(s.len());
    (end > 0).then(|| (&s[..end], &s[end..]))
}

/// `siftr --` execs its command directly, so anything a shell would interpret keeps its shell. Cron still turns
/// `%` into a newline before the shell sees it, inside the quotes as outside.
pub fn wrap(job: &Job, siftr: &str) -> String {
    let shell = job
        .command
        .bytes()
        .any(|b| b"|&;<>()$`\\\"'*?[]#~%{}!".contains(&b))
        || job
            .command
            .split_whitespace()
            .next()
            .is_some_and(|word| word.contains('='));
    let command = match shell {
        true => format!("sh -c '{}'", job.command.replace('\'', r"'\''")),
        false => job.command.clone(),
    };
    let user = job
        .user
        .as_deref()
        .map_or(String::new(), |u| format!("{u} "));
    format!(
        "{} {user}{} --quiet-unless-changed -- {command}",
        job.schedule,
        shell_join(&[siftr])
    )
}

fn read_lines(path: &Path) -> io::Result<Vec<String>> {
    Ok(std::fs::read_to_string(path)?
        .lines()
        .map(str::to_owned)
        .collect())
}

/// Counts matching lines without holding the file: a syslog can be large.
fn count_lines(path: &Path, matches: impl Fn(&str) -> bool) -> io::Result<u64> {
    let mut count = 0;
    for line in BufReader::new(File::open(path)?).split(b'\n') {
        if matches(&String::from_utf8_lossy(&line?)) {
            count += 1;
        }
    }
    Ok(count)
}

fn unified_log(command: &[OsString]) -> io::Result<u64> {
    let (program, args) = command.split_first().expect("a log command");
    let out = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "{} exited {}",
            Path::new(program).display(),
            out.status
        )));
    }
    // `log show` heads its output with a column header, even when nothing matched.
    let lines = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.starts_with("Timestamp "))
        .count();
    Ok(lines as u64)
}

fn counted(source: String, lines: io::Result<u64>, empty: &str) -> Evidence {
    let (status, lines) = match lines {
        Ok(0) => (Status::Empty(empty.to_owned()), 0),
        Ok(n) => (Status::Found, n),
        Err(error) => (unreadable(&error), 0),
    };
    Evidence {
        source,
        status,
        lines,
    }
}

fn unreadable(error: &io::Error) -> Status {
    match error.kind() {
        io::ErrorKind::NotFound => Status::Absent,
        io::ErrorKind::PermissionDenied => Status::Unreadable("permission denied".to_owned()),
        _ => Status::Unreadable(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIFTR: &str = "/opt/bin/siftr";

    fn job(line: &str, with_user: bool) -> Option<Job> {
        parse(&[line], with_user, SIFTR).pop()
    }

    #[test]
    fn crontab_entries_are_jobs_and_comments_and_environment_are_not() {
        let lines = [
            "# nightly",
            "MAILTO=me",
            "",
            "0 3 * * * /usr/local/bin/backup.sh --full",
            "@hourly sync-notes",
        ];
        let jobs = parse(&lines, false, SIFTR);
        let found: Vec<_> = jobs
            .iter()
            .map(|j| (j.schedule.as_str(), j.command.as_str()))
            .collect();
        assert_eq!(
            found,
            [
                ("0 3 * * *", "/usr/local/bin/backup.sh --full"),
                ("@hourly", "sync-notes")
            ]
        );
        let system = job("*/5 * * * * root  run-parts /etc/cron.hourly", true).unwrap();
        assert_eq!(
            (system.user.as_deref(), system.command.as_str()),
            (Some("root"), "run-parts /etc/cron.hourly")
        );
        assert_eq!(
            job("0 3 * * *", false),
            None,
            "a schedule without a command"
        );
    }

    #[test]
    fn a_wrapped_line_keeps_the_schedule_and_the_shell_the_job_needs() {
        let wrapped = |line: &str, with_user| job(line, with_user).unwrap().wrapped.unwrap();
        assert_eq!(
            wrapped("0 3 * * * /usr/local/bin/backup.sh --full", false),
            "0 3 * * * /opt/bin/siftr --quiet-unless-changed -- /usr/local/bin/backup.sh --full"
        );
        assert_eq!(
            wrapped("@daily root cleanup", true),
            "@daily root /opt/bin/siftr --quiet-unless-changed -- cleanup"
        );
        assert_eq!(
            wrapped(
                "0 * * * * cd ~/notes && git commit -qam 'auto' >/dev/null",
                false
            ),
            r"0 * * * * /opt/bin/siftr --quiet-unless-changed -- sh -c 'cd ~/notes && git commit -qam '\''auto'\'' >/dev/null'"
        );
        assert_eq!(
            wrapped("0 0 * * * date +\\%F", false),
            r"0 0 * * * /opt/bin/siftr --quiet-unless-changed -- sh -c 'date +\%F'"
        );
        assert_eq!(
            wrapped("0 0 * * * LANG=C sort x", false),
            "0 0 * * * /opt/bin/siftr --quiet-unless-changed -- sh -c 'LANG=C sort x'"
        );
    }

    #[test]
    fn discovery_reads_each_place_and_says_what_it_found_there() {
        let dir = tempfile::tempdir().unwrap();
        let path = |name: &str| dir.path().join(name);
        std::fs::create_dir(path("cron.d")).unwrap();
        std::fs::write(
            path("cron.d/backup"),
            "SHELL=/bin/sh\n15 2 * * * root /usr/sbin/backup\n",
        )
        .unwrap();
        std::fs::create_dir(path("agents")).unwrap();
        std::fs::write(
            path("agents/com.example.sync.plist"),
            "<key>StartCalendarInterval</key>",
        )
        .unwrap();
        std::fs::write(
            path("agents/com.example.daemon.plist"),
            "<key>KeepAlive</key>",
        )
        .unwrap();
        std::fs::write(
            path("mail"),
            "From cron\nSubject: Cron <me@host> backup\n\nexit 1\nSubject: hi\n",
        )
        .unwrap();
        let places = Places {
            crontab: ["sh", "-c", "echo 'crontab: no crontab for me' >&2; exit 1"]
                .map(OsString::from)
                .to_vec(),
            system_crontab: path("crontab"),
            cron_d: path("cron.d"),
            launch_agents: Some(path("agents")),
            mail_spool: Some(path("mail")),
            cron_logs: vec![path("cron.log")],
            unified_log: Some(
                ["sh", "-c", "echo 'Timestamp   Ty Process'"]
                    .map(OsString::from)
                    .to_vec(),
            ),
            siftr: SIFTR.to_owned(),
        };
        let found = Discovery::of(&places);
        let jobs: Vec<_> = found
            .jobs
            .iter()
            .map(|s| (s.status.as_str(), s.jobs.len()))
            .collect();
        assert_eq!(
            jobs,
            [("empty", 0), ("absent", 0), ("found", 1), ("found", 1)]
        );
        assert_eq!(
            found.jobs[0].status.detail(),
            Some("no crontab for this user")
        );
        assert_eq!(found.jobs[3].jobs[0].command, "com.example.sync");
        let evidence: Vec<_> = found
            .evidence
            .iter()
            .map(|e| (e.status.as_str(), e.lines))
            .collect();
        assert_eq!(
            evidence,
            [("found", 1), ("absent", 0), ("empty", 0)],
            "the log header isn't a line from cron"
        );
        assert!(found.any());
    }
}
