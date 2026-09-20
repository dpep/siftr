//! The Rails log. SQL lines become `db.query` behaviors keyed by query name and statement; each
//! `Started … Completed` block becomes one `http.request` keyed by method, controller#action and
//! status class; every other line is a `log` behavior.
//!
//! Requests key on controller#action, which Rails already resolved from the route, so no path shaping is needed.
//!
//! A block's lines are scoped to the enclosing test example when there is one, and otherwise to the block
//! itself: outside a test run, the request is the unit its queries and log lines belong to.

use std::str::FromStr;
use std::time::Duration;

use super::{Event, Outcome, generic, literal};
use crate::behavior::{BehaviorId, Kind};
use crate::normalize::{Normalizer, strip_ansi};
use crate::observation::Observation;

#[derive(Default)]
pub(super) struct Log {
    /// The current line without ANSI escapes.
    clean: Vec<u8>,
    /// A query's template input: its name and statement, without the trailing binds.
    query: Vec<u8>,
    template: Vec<u8>,
    request: Option<Request>,
}

struct Request {
    method: Vec<u8>,
    /// The `controller#action` of `Processing by`, with the endpoint id this block's lines scope to.
    /// `None` until that line: a block that never names an action scopes nothing and emits no request.
    action: Option<(Vec<u8>, BehaviorId)>,
    /// The scope of the `Started` line.
    scope: Option<BehaviorId>,
    /// Counted queries, for a `Completed` line that doesn't report its own count.
    queries: u64,
}

impl Request {
    /// What this block's lines are scoped to when no example encloses them.
    fn endpoint(&self) -> Option<BehaviorId> {
        self.action.as_ref().map(|&(_, id)| id)
    }
}

impl Log {
    pub(super) fn line(
        &mut self,
        obs: Observation<'_>,
        scope: Option<BehaviorId>,
        normalizer: &mut Normalizer,
        emit: &mut impl FnMut(&Event<'_>),
    ) {
        strip_ansi(obs.line, &mut self.clean);
        let text = trim(&self.clean);
        // The enclosing example when a test run named one, else the request block this line falls in.
        // An example is the finer unit, so a request scope never displaces one.
        let scope = scope.or_else(|| self.request.as_ref().and_then(Request::endpoint));
        if let Some(query) = Query::parse(text) {
            if let Some(request) = &mut self.request
                && query.counts()
            {
                request.queries += 1;
            }
            self.query.clear();
            self.query.extend_from_slice(query.name);
            self.query.push(b' ');
            collapse_placeholders(query.sql, &mut self.query);
            emit(&Event {
                kind: Kind::DbQuery,
                template: normalizer.normalize(&self.query),
                input: &self.query,
                source: obs,
                duration: millis(query.ms),
                outcome: None,
                scope,
                measures: &[],
            });
        } else if let Some(method) = started(text) {
            // A request that never completed (e.g. a routing error) is dropped; its error lines remain as logs.
            self.request = Some(Request {
                method: method.to_vec(),
                action: None,
                scope,
                queries: 0,
            });
        } else if let (Some(action), Some(request)) = (processing(text), &mut self.request) {
            // The block's scope is fixed here, not at `Completed`: every line it scopes comes first, while
            // the status class that completes the request's own behavior comes last. So the scope is the
            // endpoint rather than that behavior, and names no behavior of its own.
            self.template.clear();
            self.template.extend_from_slice(&request.method);
            self.template.push(b' ');
            self.template.extend_from_slice(action);
            let endpoint = BehaviorId::of(Kind::HttpRequest, &self.template);
            request.action = Some((action.to_vec(), endpoint));
        } else if let Some(completed) = Completed::parse(text)
            && let Some(request) = self.request.take()
            && let Some((action, _)) = &request.action
        {
            self.template.clear();
            self.template.extend_from_slice(&request.method);
            self.template.push(b' ');
            self.template.extend_from_slice(action);
            self.template.extend_from_slice(&[
                b' ',
                b'0' + (completed.status / 100) as u8,
                b'x',
                b'x',
            ]);
            let queries = completed.queries.unwrap_or(request.queries);
            emit(&Event {
                kind: Kind::HttpRequest,
                template: literal(&self.template),
                input: &self.template,
                source: obs,
                duration: millis(completed.ms),
                outcome: Some(if completed.status >= 500 {
                    Outcome::Failure
                } else {
                    Outcome::Success
                }),
                scope: request.scope.or_else(|| request.endpoint()),
                measures: &[("queries", queries as f64)],
            });
        } else {
            generic::log(obs, scope, normalizer, emit);
        }
    }
}

/// `User Load (0.1ms)  SELECT …  [["id", 1]]`, after ANSI stripping.
struct Query<'l> {
    name: &'l [u8],
    ms: f64,
    sql: &'l [u8],
}

impl<'l> Query<'l> {
    fn parse(text: &'l [u8]) -> Option<Self> {
        let open = text.iter().position(|&b| b == b'(')?;
        let name = trim(&text[..open]);
        let name_like =
            |&b: &u8| b.is_ascii_alphanumeric() || matches!(b, b' ' | b':' | b'?' | b'_');
        if !name.iter().all(name_like) {
            return None;
        }
        let close = open + text[open..].iter().position(|&b| b == b')')?;
        let ms = parse_ms(&text[open + 1..close])?;
        let rest = text[close + 1..].strip_prefix(b" ")?;
        let sql = find(rest, b"  [[").map_or(rest, |binds| &rest[..binds]);
        let sql = trim(sql);
        (!sql.is_empty()).then_some(Query { name, ms, sql })
    }

    /// Whether Rails' `(N queries)` counts it: transaction and schema statements aren't.
    fn counts(&self) -> bool {
        !matches!(self.name, b"TRANSACTION" | b"SCHEMA")
    }
}

/// `Completed 200 OK in 4ms (Views: 3.4ms | ActiveRecord: 0.1ms (3 queries, 0 cached) | GC: 0.0ms)`.
struct Completed {
    status: u16,
    ms: f64,
    queries: Option<u64>,
}

impl Completed {
    fn parse(text: &[u8]) -> Option<Self> {
        let rest = text.strip_prefix(b"Completed ")?;
        let status = parse_ascii(rest.get(..3)?).filter(|status| (100..1000).contains(status))?;
        let timing = &rest[find(rest, b" in ")? + 4..];
        let ms = parse_ms(
            &timing[..timing
                .iter()
                .position(|&b| b == b' ')
                .unwrap_or(timing.len())],
        )?;
        let queries = find(rest, b" queries").and_then(|at| {
            let before = &rest[..at];
            let digits = before
                .iter()
                .rposition(|b| !b.is_ascii_digit())
                .map_or(0, |p| p + 1);
            parse_ascii(&before[digits..])
        });
        Some(Completed {
            status,
            ms,
            queries,
        })
    }
}

/// The method of `Started GET "/users/1" for …`.
fn started(text: &[u8]) -> Option<&[u8]> {
    let rest = text.strip_prefix(b"Started ")?;
    let method = &rest[..rest.iter().position(|&b| b == b' ')?];
    (!method.is_empty() && method.iter().all(u8::is_ascii_uppercase)).then_some(method)
}

/// The controller#action of `Processing by UsersController#show as HTML`.
fn processing(text: &[u8]) -> Option<&[u8]> {
    let rest = text.strip_prefix(b"Processing by ")?;
    Some(find(rest, b" as ").map_or(rest, |at| &rest[..at]))
}

/// `IN (?, ?, ?)` and `IN ($1, $2)` both become `IN (?)`: a bind list's length is data, not another query.
fn collapse_placeholders(sql: &[u8], out: &mut Vec<u8>) {
    let mut i = 0;
    while i < sql.len() {
        let len = placeholder_len(sql, i);
        if len == 0 {
            out.push(sql[i]);
            i += 1;
            continue;
        }
        let kept = out.iter().rposition(|&b| b != b' ').map_or(0, |p| p + 1);
        if out[..kept].ends_with(b"?,") {
            out.truncate(kept - 1);
        } else {
            out.push(b'?');
        }
        i += len;
    }
}

fn placeholder_len(sql: &[u8], i: usize) -> usize {
    match sql[i] {
        b'?' => 1,
        b'$' if i == 0 || !(sql[i - 1].is_ascii_alphanumeric() || sql[i - 1] == b'_') => {
            match sql[i + 1..]
                .iter()
                .take_while(|b| b.is_ascii_digit())
                .count()
            {
                0 => 0,
                digits => 1 + digits,
            }
        }
        _ => 0,
    }
}

/// `0.1ms` → 0.1.
fn parse_ms(t: &[u8]) -> Option<f64> {
    let number = t.strip_suffix(b"ms")?;
    if !number.first().is_some_and(u8::is_ascii_digit) {
        return None;
    }
    parse_ascii(number)
}

fn parse_ascii<T: FromStr>(t: &[u8]) -> Option<T> {
    std::str::from_utf8(t).ok()?.parse().ok()
}

fn millis(ms: f64) -> Option<Duration> {
    Duration::try_from_secs_f64(ms / 1e3).ok()
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn trim(t: &[u8]) -> &[u8] {
    let start = t.iter().position(|&b| b != b' ').unwrap_or(t.len());
    let end = t.iter().rposition(|&b| b != b' ').map_or(start, |p| p + 1);
    &t[start..end]
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::interpret;
    use super::super::rspec::LOG_STREAM;
    use super::*;

    #[test]
    fn query_lines_key_on_name_and_statement_without_binds() {
        let cases = [
            (
                "  \x1b[1m\x1b[36mComment Load (0.1ms)\x1b[0m  \x1b[1m\x1b[34mSELECT \"comments\".* FROM \"comments\" WHERE \"comments\".\"post_id\" IN (?, ?, ?)\x1b[0m  [[\"post_id\", 1], [\"post_id\", 2], [\"post_id\", 3]]",
                Some(
                    "Comment Load SELECT \"comments\".* FROM \"comments\" WHERE \"comments\".\"post_id\" IN (?)",
                ),
            ),
            (
                "  Comment Load (0.4ms)  SELECT \"comments\".* FROM \"comments\" WHERE \"comments\".\"post_id\" IN ($1, $2)  [[\"post_id\", 1], [\"post_id\", 2]]",
                Some(
                    "Comment Load SELECT \"comments\".* FROM \"comments\" WHERE \"comments\".\"post_id\" IN (?)",
                ),
            ),
            (
                "  CACHE User Load (0.0ms)  SELECT \"users\".* FROM \"users\" WHERE \"users\".\"id\" = $1 LIMIT $2  [[\"id\", 1], [\"LIMIT\", 1]]",
                Some(
                    "CACHE User Load SELECT \"users\".* FROM \"users\" WHERE \"users\".\"id\" = ? LIMIT ?",
                ),
            ),
            (
                "  User Update (0.1ms)  UPDATE \"users\" SET \"name\" = ?, \"updated_at\" = ? WHERE \"users\".\"id\" = ?  [[\"name\", \"Bo\"]]",
                Some(
                    "User Update UPDATE \"users\" SET \"name\" = ?, \"updated_at\" = ? WHERE \"users\".\"id\" = ?",
                ),
            ),
            (
                "  TRANSACTION (0.0ms)  SAVEPOINT active_record_1",
                Some("TRANSACTION SAVEPOINT active_record_<int>"),
            ),
            (
                "  User Exists? (0.3ms)  SELECT 1 AS one FROM \"users\" WHERE \"users\".\"email\" = 'a@b.co' LIMIT 1",
                Some(
                    "User Exists? SELECT <int> AS one FROM \"users\" WHERE \"users\".\"email\" = <quoted> LIMIT <int>",
                ),
            ),
            (
                "  Rendered users/show.html.erb within layouts/application (Duration: 3.5ms | GC: 0.0ms)",
                None,
            ),
            ("  Rendered users/_user.html.erb (0.3ms)", None),
            ("  Parameters: {\"id\" => \"1\"}", None),
        ];
        for (line, expected) in cases {
            let [seen] = interpret(&[(LOG_STREAM, line.as_bytes())])
                .try_into()
                .expect("one event");
            match expected {
                Some(template) => {
                    assert_eq!(
                        (seen.kind, seen.template.as_str()),
                        (Kind::DbQuery, template)
                    );
                }
                None => assert_eq!(seen.kind, Kind::Log, "{line}"),
            }
        }
    }

    #[test]
    fn a_request_block_is_one_behavior_carrying_the_reported_query_count() {
        let log = "Started GET \"/users/1\" for 127.0.0.1 at 2026-09-13 17:03:42 -0700\n\
            Processing by UsersController#show as HTML\n  \
            User Load (0.0ms)  SELECT \"users\".* FROM \"users\" WHERE \"users\".\"id\" = ? LIMIT ?  [[\"id\", 1], [\"LIMIT\", 1]]\n  \
            Rendered users/show.html.erb within layouts/application (Duration: 3.3ms | GC: 0.0ms)\n\
            Completed 200 OK in 4ms (Views: 3.4ms | ActiveRecord: 0.1ms (7 queries, 2 cached) | GC: 0.0ms)\n";
        let seen = interpret(&[(LOG_STREAM, log.as_bytes())]);
        let kinds: Vec<_> = seen.iter().map(|s| s.kind).collect();
        assert_eq!(kinds, [Kind::DbQuery, Kind::Log, Kind::HttpRequest]);
        let request = &seen[2];
        assert_eq!(request.template, "GET UsersController#show 2xx");
        assert_eq!(request.duration, Some(Duration::from_millis(4)));
        assert_eq!(request.outcome, Some(Outcome::Success));
        assert_eq!(request.measures, [("queries", 7.0)]);
    }

    #[test]
    fn without_a_reported_count_a_request_counts_its_queries_but_not_transactions() {
        let log = "Started POST \"/users\" for ::1 at 2026-09-13 17:03:42 -0700\n\
            Processing by UsersController#create as JSON\n  \
            TRANSACTION (0.0ms)  BEGIN\n  \
            User Create (0.2ms)  INSERT INTO \"users\" (\"name\") VALUES (?) RETURNING \"id\"  [[\"name\", \"Ada\"]]\n  \
            User Load (0.1ms)  SELECT \"users\".* FROM \"users\"\n\
            Completed 500 Internal Server Error in 12ms (ActiveRecord: 0.3ms)\n";
        let seen = interpret(&[(LOG_STREAM, log.as_bytes())]);
        let request = seen.last().expect("events");
        assert_eq!(
            (request.kind, request.template.as_str()),
            (Kind::HttpRequest, "POST UsersController#create 5xx")
        );
        assert_eq!(request.outcome, Some(Outcome::Failure));
        assert_eq!(request.measures, [("queries", 2.0)]);
    }

    #[test]
    fn a_blocks_lines_scope_to_its_endpoint_when_no_example_encloses_them() {
        let log = "Started GET \"/posts\" for 127.0.0.1 at 2026-09-19 19:00:14 -0700\n\
            Processing by PostsController#index as HTML\n  \
            Post Load (0.0ms)  SELECT \"posts\".* FROM \"posts\"\n  \
            \u{21b3} app/views/posts/index.html.erb:<int>\n\
            Completed 200 OK in 10ms (Views: 7.3ms | ActiveRecord: 0.7ms (1 queries, 0 cached))\n\
            [ActiveJob] enqueued CleanupJob\n";
        let seen = interpret(&[(LOG_STREAM, log.as_bytes())]);
        let endpoint = BehaviorId::of(Kind::HttpRequest, b"GET PostsController#index");
        let scoped: Vec<_> = seen
            .iter()
            .map(|s| (s.kind, s.scope == Some(endpoint)))
            .collect();
        assert_eq!(
            scoped,
            [
                (Kind::DbQuery, true),
                (Kind::Log, true),
                (Kind::HttpRequest, true),
                (Kind::Log, false),
            ],
            "the block's lines and the request itself take the endpoint; the line after it does not"
        );
        let request = seen
            .iter()
            .find(|s| s.kind == Kind::HttpRequest)
            .expect("the completed request");
        assert_eq!(request.template, "GET PostsController#index 2xx");
        assert_ne!(
            request.id(),
            endpoint,
            "the endpoint is not the request's own behavior: its status class arrives last"
        );
    }

    #[test]
    fn a_completed_line_without_its_request_is_a_log() {
        let seen = interpret(&[(
            LOG_STREAM,
            b"Completed 200 OK in 4ms (Views: 1ms)".as_slice(),
        )]);
        assert_eq!(seen.iter().map(|s| s.kind).collect::<Vec<_>>(), [Kind::Log]);
    }
}
