//! What siftr stores of a command's output: `SIFTR_REDACT` and `SIFTR_CAPTURE`. Neither changes what the
//! terminal shows, nor behavior ids: templates mask credentials under every setting.

use siftr::normalize::secrets::Mode;

use crate::output;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Privacy {
    /// `SIFTR_REDACT`: what raw captures and kept lines keep.
    pub redact: Mode,
    /// `SIFTR_CAPTURE`: whether each run's raw output is written to disk. Without it, kept lines remain, but
    /// `explain` and `evidence` can't read a failure's whole message back.
    pub capture: bool,
}

impl Privacy {
    pub fn from_env() -> Self {
        Self::from_vars(|name| std::env::var(name).ok(), output::warn)
    }

    /// An unknown value warns and keeps the default: a typo must not stop the command, nor store more than meant.
    fn from_vars(var: impl Fn(&str) -> Option<String>, mut warn: impl FnMut(String)) -> Self {
        let mut setting = |name: &str, expected: &str| {
            let value = var(name)?;
            let known = match (name, value.trim()) {
                ("SIFTR_CAPTURE", "on" | "off") => true,
                ("SIFTR_REDACT", v) => Mode::parse(v).is_some(),
                _ => false,
            };
            if !known {
                warn(format!(
                    "{name}={value} is not {expected}; using the default"
                ));
            }
            known.then(|| value.trim().to_owned())
        };
        let redact = setting("SIFTR_REDACT", "secrets, pii or off");
        let capture = setting("SIFTR_CAPTURE", "on or off");
        Privacy {
            redact: redact.as_deref().and_then(Mode::parse).unwrap_or_default(),
            capture: capture.as_deref() != Some("off"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn privacy(vars: &[(&str, &str)]) -> (Privacy, Vec<String>) {
        let mut warnings = Vec::new();
        let privacy = Privacy::from_vars(
            |name| {
                vars.iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, v)| (*v).to_owned())
            },
            |message| warnings.push(message),
        );
        (privacy, warnings)
    }

    #[test]
    fn secrets_are_redacted_and_captures_kept_by_default() {
        let expected = Privacy {
            redact: Mode::Secrets,
            capture: true,
        };
        assert_eq!(privacy(&[]), (expected, Vec::new()));
        let (set, warnings) = privacy(&[("SIFTR_REDACT", "pii"), ("SIFTR_CAPTURE", "off")]);
        assert_eq!(
            (set.redact, set.capture, warnings.len()),
            (Mode::Pii, false, 0)
        );
        assert_eq!(privacy(&[("SIFTR_REDACT", " off ")]).0.redact, Mode::Off);
    }

    #[test]
    fn an_unknown_value_warns_and_keeps_the_default() {
        let (set, warnings) = privacy(&[("SIFTR_REDACT", "none"), ("SIFTR_CAPTURE", "no")]);
        assert_eq!((set.redact, set.capture), (Mode::Secrets, true));
        assert_eq!(
            warnings,
            [
                "SIFTR_REDACT=none is not secrets, pii or off; using the default",
                "SIFTR_CAPTURE=no is not on or off; using the default"
            ]
        );
    }
}
