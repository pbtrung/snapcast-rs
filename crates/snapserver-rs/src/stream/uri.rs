//! Stream URI parser matching C++ `source=` config syntax.
//!
//! Format: `scheme:///path?key=value&key=value#fragment`
//!
//! Scheme, query and fragment come from the status parser
//! ([`snapcast_proto::status::StreamUri`]) and the path uses the same percent
//! decoder, so the source this binary opens is the one the status reports.
//! Examples:
//! - `pipe:///tmp/snapfifo?name=Radio&sampleformat=48000:16:2`
//! - `process:///usr/bin/mpd?name=MPD`
//! - `tcp://0.0.0.0:4953?name=TCP`
//! - `file:///path/to/file.wav?name=File`

use std::collections::HashMap;

use anyhow::{Context, Result};
use snapcast_proto::status::percent_decode;

/// Parsed stream URI.
#[derive(Debug, Clone)]
pub struct StreamUri {
    /// Scheme: pipe, file, process, tcp.
    pub scheme: String,
    /// Host (for tcp scheme).
    pub host: String,
    /// Port (for tcp scheme).
    pub port: u16,
    /// Path (for pipe, file, process schemes).
    pub path: String,
    /// Query parameters.
    pub query: HashMap<String, String>,
}

impl StreamUri {
    /// Parse a stream URI string.
    pub fn parse(uri: &str) -> Result<Self> {
        let uri = uri.trim().trim_matches(|c| c == '\'' || c == '"');

        let (scheme, rest) = uri
            .split_once("://")
            .with_context(|| format!("invalid stream URI: {uri}"))?;
        let shared = snapcast_proto::status::StreamUri::parse(uri);

        let rest = rest.split_once('#').map_or(rest, |(r, _)| r);
        let path_part = rest.split_once('?').map_or(rest, |(p, _)| p);

        // Parse host:port for tcp scheme
        let (host, port, path) = if scheme == "tcp" {
            let (host, port) = parse_tcp_endpoint(path_part)?;
            (host, port, String::new())
        } else {
            // For pipe/file/process: path starts after ://
            // Typically pipe:///tmp/snapfifo → path = /tmp/snapfifo
            let path = percent_decode(path_part.strip_prefix("//").unwrap_or(path_part));
            (String::new(), 0, path)
        };

        Ok(Self {
            scheme: scheme.to_string(),
            host,
            port,
            path,
            query: shared.query,
        })
    }

    /// Get a query parameter value.
    pub fn param(&self, key: &str) -> Option<&str> {
        self.query.get(key).map(|s| s.as_str())
    }
}

fn parse_tcp_endpoint(path_part: &str) -> Result<(String, u16)> {
    let endpoint = path_part.trim_start_matches('/');
    if let Some(stripped) = endpoint.strip_prefix('[') {
        let (host, tail) = stripped
            .split_once(']')
            .context("invalid TCP stream IPv6 endpoint, missing closing ']'")?;
        anyhow::ensure!(!host.is_empty(), "missing TCP stream host");
        let port = if tail.is_empty() {
            4953
        } else {
            let port_str = tail
                .strip_prefix(':')
                .with_context(|| format!("invalid TCP stream endpoint suffix: {tail}"))?;
            parse_port(port_str)?
        };
        return Ok((host.to_string(), port));
    }

    if endpoint.matches(':').count() == 1 {
        let (host, port_str) = endpoint
            .rsplit_once(':')
            .expect("counted exactly one ':' before splitting");
        anyhow::ensure!(!host.is_empty(), "missing TCP stream host");
        return Ok((host.to_string(), parse_port(port_str)?));
    }

    // No colons — plain hostname or IPv4
    if !endpoint.contains(':') {
        return Ok((endpoint.to_string(), 4953));
    }

    // Multiple colons without brackets — bare IPv6. Accept only if valid IPv6.
    // If the user intended a port, they must use bracket notation: [::1]:4953
    anyhow::ensure!(
        endpoint.parse::<std::net::Ipv6Addr>().is_ok(),
        "ambiguous IPv6 address with port — use bracket notation: tcp://[{endpoint}]:port"
    );
    Ok((endpoint.to_string(), 4953))
}

fn parse_port(port_str: &str) -> Result<u16> {
    anyhow::ensure!(!port_str.is_empty(), "missing TCP stream port");
    port_str
        .parse()
        .with_context(|| format!("invalid TCP stream port: {port_str}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pipe_uri() {
        let u =
            StreamUri::parse("pipe:///tmp/snapfifo?name=Radio&sampleformat=48000:16:2").unwrap();
        assert_eq!(u.scheme, "pipe");
        assert_eq!(u.path, "/tmp/snapfifo");
        assert_eq!(u.param("name"), Some("Radio"));
        assert_eq!(u.param("sampleformat"), Some("48000:16:2"));
    }

    #[test]
    fn parse_tcp_uri() {
        let u = StreamUri::parse("tcp://0.0.0.0:4953?name=TCP").unwrap();
        assert_eq!(u.scheme, "tcp");
        assert_eq!(u.host, "0.0.0.0");
        assert_eq!(u.port, 4953);
        assert_eq!(u.param("name"), Some("TCP"));
    }

    #[test]
    fn parse_tcp_ipv6_uri() {
        let u = StreamUri::parse("tcp://[::1]:4953?name=TCP").unwrap();
        assert_eq!(u.scheme, "tcp");
        assert_eq!(u.host, "::1");
        assert_eq!(u.port, 4953);
    }

    #[test]
    fn parse_tcp_ipv6_uri_default_port() {
        let u = StreamUri::parse("tcp://::1?name=TCP").unwrap();
        assert_eq!(u.host, "::1");
        assert_eq!(u.port, 4953);
    }

    #[test]
    fn parse_tcp_ipv6_ambiguous_with_port_rejected() {
        // "::1:99999" is not a valid IPv6 address, so it's rejected as ambiguous
        let err = StreamUri::parse("tcp://::1:99999?name=TCP").unwrap_err();
        assert!(err.to_string().contains("bracket notation"));
    }

    #[test]
    fn parse_file_uri_with_spaces() {
        let u =
            StreamUri::parse("file:///home/user/Musik/Some%20wave%20file.wav?name=File").unwrap();
        assert_eq!(u.scheme, "file");
        assert_eq!(u.path, "/home/user/Musik/Some wave file.wav");
        assert_eq!(u.param("name"), Some("File"));
    }

    #[test]
    fn parse_decodes_utf8_paths_and_keeps_malformed_escapes() {
        let u = StreamUri::parse("file:///music/Über%20Alles/caf%C3%A9.wav?name=File").unwrap();
        assert_eq!(u.path, "/music/Über Alles/café.wav");
        let u = StreamUri::parse("pipe:///tmp/a%zzb%2?name=x").unwrap();
        assert_eq!(u.path, "/tmp/a%zzb%2", "malformed escapes stay literal");
    }

    #[test]
    fn parse_decodes_query_keys_and_drops_fragment() {
        let u = StreamUri::parse("pipe:///tmp/f?na%6De=Radio%20One&x=%E2%82%AC#frag").unwrap();
        assert_eq!(u.param("name"), Some("Radio One"));
        assert_eq!(u.param("x"), Some("€"), "fragment is not part of the value");
        assert_eq!(u.path, "/tmp/f");
    }

    #[test]
    fn parse_agrees_with_status_parser() {
        let raw = "pipe:///tmp/sn%C3%A4p?name=K%C3%BCche&sampleformat=48000:16:2#f";
        let ours = StreamUri::parse(raw).unwrap();
        let status = snapcast_proto::status::StreamUri::parse(raw);
        assert_eq!(ours.path, status.path);
        assert_eq!(ours.query, status.query);
    }

    #[test]
    fn parse_process_uri() {
        let u =
            StreamUri::parse("process:///usr/bin/mpd?name=MPD&sampleformat=44100:16:2").unwrap();
        assert_eq!(u.scheme, "process");
        assert_eq!(u.path, "/usr/bin/mpd");
    }
}
