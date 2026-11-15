//! `buzz guide` — on-demand guidance for agents (local, no relay connection).
//!
//! - `buzz guide`          — list the topics and when to open each
//! - `buzz guide <topic>`  — print the topic's markdown to stdout
//!
//! The text is compiled in from `buzz_sdk::guides`, the same source the
//! `buzz-acp` base prompt indexes, so the two cannot drift.

use std::io::Write;

use buzz_sdk::guides;

use crate::error::CliError;

/// Run `buzz guide [topic]`, writing to the process stdout and stderr.
pub fn cmd_guide(topic: Option<&str>) -> Result<(), CliError> {
    run(
        topic,
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    )
}

/// Write the topic list or one topic to `out`. An unknown topic writes the
/// topic list to `err` and returns a usage error (exit code 1).
fn run(topic: Option<&str>, out: &mut impl Write, err: &mut impl Write) -> Result<(), CliError> {
    let write_failed = |e: std::io::Error| CliError::Other(format!("write failed: {e}"));
    match topic {
        None => out
            .write_all(guides::list().as_bytes())
            .map_err(write_failed),
        Some(name) => match guides::find(name) {
            Some(guide) => out.write_all(guide.text.as_bytes()).map_err(write_failed),
            None => {
                // Best effort: the usage error below is what the caller acts on.
                let _ = write!(err, "Available topics:\n{}", guides::list());
                Err(CliError::Usage(format!(
                    "unknown guide topic `{name}`; run `buzz guide` to list topics"
                )))
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_capture(topic: Option<&str>) -> (Result<(), CliError>, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let result = run(topic, &mut out, &mut err);
        (
            result,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[test]
    fn list_without_topic_names_every_topic_on_stdout() {
        let (result, out, err) = run_capture(None);
        assert!(result.is_ok());
        assert!(err.is_empty());
        for guide in guides::GUIDES {
            assert!(out.contains(guide.name), "list is missing {}", guide.name);
        }
    }

    #[test]
    fn each_topic_prints_its_exact_text() {
        for guide in guides::GUIDES {
            let (result, out, err) = run_capture(Some(guide.name));
            assert!(result.is_ok(), "topic {}", guide.name);
            assert_eq!(out, guide.text, "topic {}", guide.name);
            assert!(err.is_empty());
        }
    }

    #[test]
    fn unknown_topic_is_a_usage_error_with_the_list_on_stderr() {
        let (result, out, err) = run_capture(Some("nope"));
        let error = result.unwrap_err();
        assert!(matches!(error, CliError::Usage(_)));
        assert_eq!(crate::error::exit_code(&error), 1);
        assert!(out.is_empty());
        for guide in guides::GUIDES {
            assert!(err.contains(guide.name), "stderr is missing {}", guide.name);
        }
    }
}
