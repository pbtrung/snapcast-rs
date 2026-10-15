//! Logging setup — wires `--logsink` and `--logfilter` into tracing.
//!
//! Sink options: `stdout`, `stderr`, `null`, `system`, `file:<path>`
//! Filter format: `<tag>:<level>[,<tag>:<level>]*`
//!   - tag: `*` (all), a component like `Stream`, `Controller`, `Stats`, or a
//!     tracing target
//!   - level: trace, debug, info, notice, warning, error, fatal

use std::io::IsTerminal;

use anyhow::{Result, bail};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::writer::BoxMakeWriter;

/// Convert snapcast-style log filter to tracing EnvFilter syntax.
///
/// Snapcast: `*:info,Stream:debug` → tracing: `info,snapcast_client::stream=debug`.
/// A part without a tag sets the level of all, as C++ snapclient. `None` on
/// an unknown level.
fn convert_filter(filter: &str) -> Option<String> {
    filter
        .split(',')
        .map(|part| {
            let (tag, level) = part.split_once(':').unwrap_or(("*", part));

            let level = match level {
                "fatal" => "error",
                "warning" => "warn",
                "notice" => "info",
                "trace" | "debug" | "info" | "warn" | "error" | "off" => level,
                _ => return None,
            };

            Some(if tag == "*" {
                level.to_string()
            } else {
                let tag_lower = tag.to_lowercase();
                // Tracing targets: module paths in the client library, plus
                // the literal `Stats` target of the stream's sync statistics.
                let module = match tag_lower.as_str() {
                    "stream" => "snapcast_client::stream",
                    "controller" => "snapcast_client::controller",
                    "connection" => "snapcast_client::connection",
                    "timeprovider" => "snapcast_client::time_provider",
                    "player" | "coreaudioplayer" | "alsaplayer" | "pulseplayer" => {
                        "snapclient_rs::player"
                    }
                    "mixer" => "snapclient_rs::mixer",
                    "flac" | "flacdecoder" => "snapcast_client::decoder::flac",
                    "opus" | "opusdecoder" => "snapcast_client::decoder::opus",
                    "stats" | "latency" => "Stats",
                    _ => tag,
                };
                format!("{module}={level}")
            })
        })
        .collect::<Option<Vec<_>>>()
        .map(|parts| parts.join(","))
}

/// Initialize logging from CLI options.
///
/// Falls back to `RUST_LOG` env var if set, otherwise uses the provided filter.
pub(crate) fn init(sink: &str, filter: &str) -> Result<()> {
    let (env_filter, invalid_filter) = if std::env::var("RUST_LOG").is_ok() {
        (EnvFilter::from_default_env(), false)
    } else {
        match convert_filter(filter).and_then(|f| EnvFilter::try_new(f).ok()) {
            Some(f) => (f, false),
            None => (EnvFilter::new("info"), true),
        }
    };

    // Colors only on a terminal, not in files or the systemd journal.
    let (writer, ansi) = match sink {
        "null" => (BoxMakeWriter::new(std::io::sink), false),
        "stdout" | "" => (
            BoxMakeWriter::new(std::io::stdout),
            std::io::stdout().is_terminal(),
        ),
        // On Unix, "system" means syslog. tracing doesn't have native syslog,
        // so it falls back to stderr.
        "stderr" | "system" => (
            BoxMakeWriter::new(std::io::stderr),
            std::io::stderr().is_terminal(),
        ),
        s if s.starts_with("file:") => {
            let path = &s[5..];
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|e| anyhow::anyhow!("failed to open log file {path}: {e}"))?;
            (BoxMakeWriter::new(file), false)
        }
        other => {
            bail!("invalid log sink: {other} (expected stdout|stderr|null|system|file:<path>)")
        }
    };
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_writer(writer)
        .with_ansi(ansi)
        .init();

    if sink == "system" {
        tracing::debug!("Log sink 'system' mapped to stderr (syslog not available)");
    }
    if invalid_filter {
        tracing::warn!(filter, "Invalid log filter, using *:info");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convert_default_filter() {
        assert_eq!(convert_filter("*:info").unwrap(), "info");
    }

    #[test]
    fn convert_multi_filter() {
        let result = convert_filter("*:info,Stream:debug").unwrap();
        assert_eq!(result, "info,snapcast_client::stream=debug");
    }

    #[test]
    fn convert_warning_to_warn() {
        assert_eq!(convert_filter("*:warning").unwrap(), "warn");
    }

    #[test]
    fn convert_fatal_to_error() {
        assert_eq!(convert_filter("*:fatal").unwrap(), "error");
    }

    #[test]
    fn convert_notice_to_info() {
        assert_eq!(convert_filter("*:notice").unwrap(), "info");
    }

    #[test]
    fn convert_controller_tag() {
        let result = convert_filter("Controller:trace").unwrap();
        assert_eq!(result, "snapcast_client::controller=trace");
    }

    #[test]
    fn convert_stats_tag_to_stats_target() {
        assert_eq!(convert_filter("Stats:debug").unwrap(), "Stats=debug");
    }

    #[test]
    fn untagged_level_applies_to_all() {
        assert_eq!(
            convert_filter("debug,Stream:trace").unwrap(),
            "debug,snapcast_client::stream=trace"
        );
    }

    #[test]
    fn unknown_level_is_rejected() {
        assert_eq!(convert_filter("*:bogus"), None);
        assert_eq!(convert_filter("Stream:loud"), None);
    }
}
