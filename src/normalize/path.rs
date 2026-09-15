//! Paths in a line: where a run happened is canonicalized, what the path names is kept.
//!
//! A path's machine-specific prefix becomes a marker (`<root>`, `~`, `<tmp>`, `<gem:name>`, `<crate:name>`,
//! `<npm:pkg>`) and its tail stays in the template, masked like any other text. A laptop's and CI's copy of one
//! deprecation warning are one behavior, and the file it points at is still that behavior's identity.

use std::fmt;
use std::ops::{BitOr, BitOrAssign};
use std::str::FromStr;

use crate::normalize::recognize::leading_digits;

/// Directories a normalizer canonicalizes beyond those every machine spells alike: a home under `/Users` or
/// `/home`, and a temp dir at `/tmp`, `/private/tmp` or `/var/folders/<a>/<b>/T`.
#[derive(Debug, Clone, Default)]
pub struct Roots {
    project: Option<Box<[u8]>>,
    home: Option<Box<[u8]>>,
    tmp: Option<Box<[u8]>>,
}

impl Roots {
    /// The run's project root, written `<root>`. It wins over a home or temp dir holding it.
    #[must_use]
    pub fn project(mut self, dir: &str) -> Self {
        self.project = absolute(dir);
        self
    }

    /// The user's home, written `~`: needed only outside `/Users` and `/home` (`/root`, `/var/lib/jenkins`).
    #[must_use]
    pub fn home(mut self, dir: &str) -> Self {
        self.home = absolute(dir);
        self
    }

    /// The temp dir (`$TMPDIR`), written `<tmp>`.
    #[must_use]
    pub fn tmp(mut self, dir: &str) -> Self {
        self.tmp = absolute(dir);
        self
    }
}

/// `dir` without trailing slashes when absolute; `/` itself would claim every path, so it's dropped too.
fn absolute(dir: &str) -> Option<Box<[u8]>> {
    let dir = dir.trim_end_matches('/');
    dir.starts_with('/').then(|| dir.as_bytes().into())
}

/// Where a path lies, as its template spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Prefix<'p> {
    Root,
    Home,
    Tmp,
    Gem(&'p [u8]),
    Crate(&'p [u8]),
    Npm(&'p [u8]),
}

impl Prefix<'_> {
    pub(crate) fn write(self, out: &mut Vec<u8>) {
        let (open, name): (&[u8], &[u8]) = match self {
            Prefix::Root => return out.extend_from_slice(b"<root>"),
            Prefix::Home => return out.push(b'~'),
            Prefix::Tmp => return out.extend_from_slice(b"<tmp>"),
            Prefix::Gem(name) => (b"<gem:", name),
            Prefix::Crate(name) => (b"<crate:", name),
            Prefix::Npm(name) => (b"<npm:", name),
        };
        out.extend_from_slice(open);
        out.extend_from_slice(name);
        out.push(b'>');
    }
}

/// An `@` in a path opens a segment (`@babel/core`); inside one it's an address (`a@b.co/x`).
pub(crate) fn ats_open_segments(t: &[u8]) -> bool {
    t.iter()
        .enumerate()
        .all(|(i, &b)| b != b'@' || i == 0 || t[i - 1] == b'/')
}

/// A path's canonical prefix and where its kept tail starts: at a `/`, or at the path's end. An installed package
/// wins (a vendored gem is the gem wherever it's installed), then the project, a home, a temp dir.
pub(crate) fn canonical<'p>(path: &'p [u8], roots: &Roots) -> Option<(Prefix<'p>, usize)> {
    if let Some(found) = dependency(path) {
        return Some(found);
    }
    let project = roots.project.as_deref();
    if let Some(n) = project.and_then(|p| under(path, p)) {
        return Some((Prefix::Root, n));
    }
    if let Some(home) = home_len(path, roots) {
        // The project spelled from any home: `~/code/app` is `/Users/dpepper/code/app`.
        let in_home = project
            .and_then(|p| home_len(p, roots).map(|h| &p[h..]))
            .filter(|t| !t.is_empty());
        return Some(match in_home.and_then(|t| under(&path[home..], t)) {
            Some(n) => (Prefix::Root, home + n),
            None => (Prefix::Home, home),
        });
    }
    tmp_len(path, roots).map(|n| (Prefix::Tmp, n))
}

/// The roles of a path left as written, or `None` unless it names a file: `/etc/hosts` reads like the route
/// `/users/new`. Only a project path takes roles from its directories: `/app/views/…` is some machine's root.
pub(crate) fn file_path_roles(path: &[u8]) -> Option<PathRoles> {
    let file = file_roles(last_segment(path))?;
    Some(match path.starts_with(b"/") || path.starts_with(b"..") {
        true => file,
        false => file | project_roles(path),
    })
}

fn names_file(path: &[u8]) -> bool {
    file_roles(last_segment(path)).is_some()
}

/// A temp dir or file stem something generated, which the next run's won't share: it has a digit, or is
/// `mktemp`'s `tmp.XXXXXXXXXX` or a `.tmpXXXXXX`.
pub(crate) fn is_generated(stem: &[u8]) -> bool {
    stem.iter().any(u8::is_ascii_digit)
        || ((stem.starts_with(b"tmp.") || stem.starts_with(b".tmp")) && stem.len() >= 10)
}

/// Length of `prefix` when `path` is it or lies below it.
fn under(path: &[u8], prefix: &[u8]) -> Option<usize> {
    let rest = path.strip_prefix(prefix)?;
    (rest.first().is_none_or(|&b| b == b'/')).then_some(prefix.len())
}

/// Where the `k` non-empty segments following `base` end.
fn below(path: &[u8], base: &[u8], k: usize) -> Option<usize> {
    let mut end = under(path, base)?;
    for _ in 0..k {
        let start = end + 1;
        if start > path.len() {
            return None;
        }
        end = segment_end(path, start);
        if end == start {
            return None;
        }
    }
    Some(end)
}

fn segment_end(path: &[u8], start: usize) -> usize {
    start
        + path[start..]
            .iter()
            .position(|&b| b == b'/')
            .unwrap_or(path.len() - start)
}

fn last_segment(path: &[u8]) -> &[u8] {
    &path[path
        .iter()
        .rposition(|&b| b == b'/')
        .map_or(0, |slash| slash + 1)..]
}

fn home_len(path: &[u8], roots: &Roots) -> Option<usize> {
    if path.first() == Some(&b'~') {
        return under(path, b"~");
    }
    roots
        .home
        .as_deref()
        .and_then(|home| under(path, home))
        .or_else(|| below(path, b"/Users", 1))
        // Rails apps route `/home/…`, so only a file below a user's home counts.
        .or_else(|| below(path, b"/home", 1).filter(|&end| end < path.len() && names_file(path)))
}

fn tmp_len(path: &[u8], roots: &Roots) -> Option<usize> {
    let folders = |base: &[u8]| {
        let (user, cache) = (below(path, base, 2)?, below(path, base, 3)?);
        (&path[user + 1..cache] == b"T").then_some(cache)
    };
    roots
        .tmp
        .as_deref()
        .and_then(|tmp| under(path, tmp))
        .or_else(|| under(path, b"/tmp"))
        .or_else(|| under(path, b"/private/tmp"))
        .or_else(|| folders(b"/var/folders"))
        .or_else(|| folders(b"/private/var/folders"))
}

/// rvm keeps each Ruby's gems under `gems/ruby-3.4.9/`: a gem home, not a gem.
const RUBIES: &[&[u8]] = &[b"ruby", b"jruby", b"truffleruby"];

/// The innermost installed package `path` lies in: a gem (rvm, rbenv, asdf, bundler), a registry crate, or an
/// npm package.
fn dependency(path: &[u8]) -> Option<(Prefix<'_>, usize)> {
    let mut found = None;
    let (mut previous, mut start): (&[u8], usize) = (b"", 0);
    while start < path.len() {
        let end = segment_end(path, start);
        let segment = &path[start..end];
        let package = match segment {
            b"gems" => package(path, end)
                .filter(|(name, _)| !RUBIES.contains(name))
                .map(|(name, e)| (Prefix::Gem(name), e)),
            b"src" if previous == b"registry" => below(path, &path[..end], 1)
                .and_then(|index| package(path, index))
                .map(|(name, e)| (Prefix::Crate(name), e)),
            b"node_modules" => npm(path, end),
            _ => None,
        };
        found = package.or(found);
        previous = segment;
        start = end + 1;
    }
    found
}

/// The name of the `<name>-<version>` directory after the `/` at `at`, and where that directory ends.
fn package(path: &[u8], at: usize) -> Option<(&[u8], usize)> {
    let end = below(path, &path[..at], 1)?;
    Some((package_name(&path[at + 1..end])?, end))
}

/// `rspec-core` of `rspec-core-3.13.6`, `nokogiri` of `nokogiri-1.16.0-arm64-darwin`, `rails` of a git checkout's
/// `rails-1a2b3c4d5e6f`. A directory with no version isn't a package.
fn package_name(dir: &[u8]) -> Option<&[u8]> {
    let versioned = (1..dir.len()).find(|&i| {
        let digits = leading_digits(&dir[i..]);
        dir[i - 1] == b'-' && digits > 0 && dir.get(i + digits) == Some(&b'.')
    });
    let revision = || {
        let dash = dir.iter().rposition(|&b| b == b'-')?;
        let rev = &dir[dash + 1..];
        (rev.len() >= 7 && rev.iter().all(u8::is_ascii_hexdigit)).then_some(dash + 1)
    };
    let at = versioned.or_else(revision)?;
    (at > 1).then(|| &dir[..at - 1])
}

fn npm(path: &[u8], at: usize) -> Option<(Prefix<'_>, usize)> {
    let mut end = below(path, &path[..at], 1)?;
    if path[at + 1] == b'@' {
        end = below(path, &path[..end], 1)?;
    }
    Some((Prefix::Npm(&path[at + 1..end]), end))
}

/// What a path is, by where it lies and what it's named. Information about a behavior, never part of its identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PathRole {
    /// `*.sqlite3`, `*.sqlite`, `*.db`, `db/*.sql`.
    Database,
    /// `*.pid`, and `*.lock` other than a manifest's lockfile.
    Lock,
    /// `Gemfile`, `Cargo.toml`, `package.json`, `go.mod`, their lockfiles, `*.gemspec`.
    Manifest,
    /// `*.log`.
    Log,
    /// Under the project's `spec/`, `test/` or `tests/`, or named `*_spec.rb` or `*_test.rb`.
    Test,
    /// Under the project's `app/`, `lib/` or `src/`.
    Source,
    /// `*.erb`, `*.haml`.
    View,
    /// Under the project's `config/`, `*.yml`, `*.yaml`, `.env*`.
    Config,
    /// Inside an installed package: `<gem:…>`, `<crate:…>`, `<npm:…>`.
    Dependency,
    /// Under a temp dir: `<tmp>`.
    Temp,
}

impl PathRole {
    pub const ALL: [PathRole; 10] = [
        PathRole::Database,
        PathRole::Lock,
        PathRole::Manifest,
        PathRole::Log,
        PathRole::Test,
        PathRole::Source,
        PathRole::View,
        PathRole::Config,
        PathRole::Dependency,
        PathRole::Temp,
    ];

    /// Persisted with behaviors and part of the JSON contract.
    pub const fn as_str(self) -> &'static str {
        match self {
            PathRole::Database => "database",
            PathRole::Lock => "lock",
            PathRole::Manifest => "manifest",
            PathRole::Log => "log",
            PathRole::Test => "test",
            PathRole::Source => "source",
            PathRole::View => "view",
            PathRole::Config => "config",
            PathRole::Dependency => "dependency",
            PathRole::Temp => "temp",
        }
    }

    const fn bit(self) -> u16 {
        1 << self as u16
    }
}

impl fmt::Display for PathRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownPathRole(pub String);

impl fmt::Display for UnknownPathRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown path role {:?}", self.0)
    }
}

impl std::error::Error for UnknownPathRole {}

impl FromStr for PathRole {
    type Err = UnknownPathRole;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        PathRole::ALL
            .into_iter()
            .find(|role| role.as_str() == s)
            .ok_or_else(|| UnknownPathRole(s.to_owned()))
    }
}

/// A set of [`PathRole`]s; displays and parses as comma-separated names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct PathRoles(u16);

impl PathRoles {
    pub fn contains(self, role: PathRole) -> bool {
        self.0 & role.bit() != 0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// In [`PathRole::ALL`] order.
    pub fn iter(self) -> impl Iterator<Item = PathRole> {
        PathRole::ALL
            .into_iter()
            .filter(move |&role| self.contains(role))
    }
}

impl From<PathRole> for PathRoles {
    fn from(role: PathRole) -> Self {
        PathRoles(role.bit())
    }
}

impl<R: Into<PathRoles>> BitOr<R> for PathRoles {
    type Output = PathRoles;

    fn bitor(self, other: R) -> PathRoles {
        PathRoles(self.0 | other.into().0)
    }
}

impl<R: Into<PathRoles>> BitOrAssign<R> for PathRoles {
    fn bitor_assign(&mut self, other: R) {
        self.0 |= other.into().0;
    }
}

impl FromIterator<PathRole> for PathRoles {
    fn from_iter<I: IntoIterator<Item = PathRole>>(iter: I) -> Self {
        iter.into_iter()
            .fold(PathRoles::default(), |set, role| set | role)
    }
}

impl fmt::Display for PathRoles {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, role) in self.iter().enumerate() {
            if i > 0 {
                f.write_str(",")?;
            }
            f.write_str(role.as_str())?;
        }
        Ok(())
    }
}

impl FromStr for PathRoles {
    type Err = UnknownPathRole;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.split(',')
            .filter(|name| !name.is_empty())
            .map(str::parse)
            .collect()
    }
}

/// The roles of a canonicalized path, from its prefix and the `tail` after it. Only a project path takes roles
/// from its directories: `~/code/app/…` names someone's project `app`, not source.
pub(crate) fn roles(prefix: Prefix<'_>, tail: &[u8]) -> PathRoles {
    let file = || file_roles(last_segment(tail)).unwrap_or_default();
    match prefix {
        Prefix::Gem(_) | Prefix::Crate(_) | Prefix::Npm(_) => PathRole::Dependency.into(),
        Prefix::Tmp => file() | PathRole::Temp,
        Prefix::Home => file(),
        Prefix::Root => file() | project_roles(tail),
    }
}

/// Roles a project path takes from its top directory.
fn project_roles(tail: &[u8]) -> PathRoles {
    let mut rest = tail;
    while let Some(inner) = rest.strip_prefix(b"/").or_else(|| rest.strip_prefix(b"./")) {
        rest = inner;
    }
    let role = match &rest[..segment_end(rest, 0)] {
        b"spec" | b"test" | b"tests" => PathRole::Test,
        b"app" | b"lib" | b"src" => PathRole::Source,
        b"config" => PathRole::Config,
        b"db" if extension(last_segment(rest)) == Some(b"sql") => PathRole::Database,
        _ => return PathRoles::default(),
    };
    role.into()
}

/// A manifest's lockfile is part of the manifest, not a runtime lock.
const MANIFESTS: &[&[u8]] = &[
    b"Gemfile",
    b"Gemfile.lock",
    b"Cargo.toml",
    b"Cargo.lock",
    b"package.json",
    b"package-lock.json",
    b"yarn.lock",
    b"go.mod",
    b"go.sum",
];

/// What a file's name says, or `None` when it names no file: no extension, and no name listed here.
fn file_roles(name: &[u8]) -> Option<PathRoles> {
    // Most segments are words or ids; `Gemfile` is the one dotless name listed.
    if !name.contains(&b'.') {
        return (name == b"Gemfile").then(|| PathRole::Manifest.into());
    }
    let ext = extension(name);
    let mut roles = match ext {
        _ if MANIFESTS.contains(&name) => PathRole::Manifest.into(),
        Some(b"gemspec") => PathRole::Manifest.into(),
        Some(b"sqlite3" | b"sqlite" | b"db") => PathRole::Database.into(),
        Some(b"lock" | b"pid") => PathRole::Lock.into(),
        Some(b"log") => PathRole::Log.into(),
        Some(b"erb" | b"haml") => PathRole::View.into(),
        Some(b"yml" | b"yaml") => PathRole::Config.into(),
        _ => PathRoles::default(),
    };
    if name == b".env" || name.starts_with(b".env.") {
        roles |= PathRole::Config;
    }
    if name.ends_with(b"_spec.rb") || name.ends_with(b"_test.rb") {
        roles |= PathRole::Test;
    }
    (ext.is_some() || !roles.is_empty()).then_some(roles)
}

/// `erb` of `show.html.erb`: letters and digits after the last dot, led by a letter. A dotfile has none.
fn extension(name: &[u8]) -> Option<&[u8]> {
    let dot = name.iter().rposition(|&b| b == b'.').filter(|&d| d > 0)?;
    let ext = &name[dot + 1..];
    let word = (1..=10).contains(&ext.len())
        && ext[0].is_ascii_alphabetic()
        && ext.iter().all(u8::is_ascii_alphanumeric);
    word.then_some(ext)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_names_drop_versions_platforms_and_revisions() {
        let cases: [(&str, Option<&str>); 7] = [
            ("activerecord-7.1.3", Some("activerecord")),
            ("rspec-core-3.13.6", Some("rspec-core")),
            ("nokogiri-1.16.0-arm64-darwin", Some("nokogiri")),
            ("aws-sdk-s3-1.2.0", Some("aws-sdk-s3")),
            ("rails-1a2b3c4d5e6f", Some("rails")),
            ("3.3.0", None),
            ("my-2fa", None),
        ];
        for (dir, name) in cases {
            assert_eq!(
                package_name(dir.as_bytes()),
                name.map(str::as_bytes),
                "{dir}"
            );
        }
    }

    #[test]
    fn roles_round_trip_through_text() {
        let roles: PathRoles = [PathRole::Temp, PathRole::Database].into_iter().collect();
        assert_eq!(roles.to_string(), "database,temp");
        assert_eq!("database,temp".parse(), Ok(roles));
        assert_eq!("".parse(), Ok(PathRoles::default()));
        assert!("database,nope".parse::<PathRoles>().is_err());
    }
}
