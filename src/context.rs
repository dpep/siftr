//! What makes runs comparable: the same project and the same command.

use std::borrow::Cow;

use crate::normalize::secrets::redact_text;

/// Baselines only ever compare runs with an equal `Context`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Context {
    project: String,
    name: String,
}

impl Context {
    /// Named by the shell-quoted command, so the name is both a stable key and pasteable.
    pub fn for_command(project: impl Into<String>, argv: &[impl AsRef<str>]) -> Self {
        Self::named(project, shell_join(argv))
    }

    /// A credential in the name is masked, the same way every time, so the name stays a stable key.
    pub fn named(project: impl Into<String>, name: impl Into<String>) -> Self {
        let mut name = name.into();
        if let Cow::Owned(redacted) = redact_text(&name) {
            name = redacted;
        }
        Context {
            project: project.into(),
            name,
        }
    }

    /// The project root the runs happened in.
    pub fn project(&self) -> &str {
        &self.project
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Joins arguments into one POSIX-shell command line, quoting only where needed.
pub fn shell_join(argv: &[impl AsRef<str>]) -> String {
    let quote = |arg: &str| {
        let plain = !arg.is_empty()
            && arg
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_./=:@%+,".contains(&b));
        if plain {
            arg.to_owned()
        } else {
            format!("'{}'", arg.replace('\'', r"'\''"))
        }
    };
    argv.iter()
        .map(|arg| quote(arg.as_ref()))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_only_what_the_shell_would_split() {
        let cases: [(&[&str], &str); 4] = [
            (
                &["bundle", "exec", "rspec", "spec/models/user_spec.rb:12"],
                "bundle exec rspec spec/models/user_spec.rb:12",
            ),
            (&["sh", "-c", "echo hi; exit 3"], "sh -c 'echo hi; exit 3'"),
            (&["echo", "it's"], r"echo 'it'\''s'"),
            (&["printf", ""], "printf ''"),
        ];
        for (argv, expected) in cases {
            assert_eq!(shell_join(argv), expected);
        }
    }

    #[test]
    fn a_credential_in_the_command_is_masked_and_still_a_stable_key() {
        let argv = [
            "env",
            concat!("API_KEY=", "Zq8vN2kLp4RxQm7Tz9Lw"),
            "bundle",
            "exec",
            "rspec",
        ];
        let context = Context::for_command("/app", &argv);
        assert_eq!(context.name(), "env API_KEY=<SECRET_1> bundle exec rspec");
        assert_eq!(context, Context::named("/app", context.name()));
        assert_eq!(context, Context::for_command("/app", &argv));
    }
}
