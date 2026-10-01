//! Config file parser for `/etc/snapserver.conf` (INI format).

use ini::Ini;

use snapcast_server::ServerConfig;

use crate::auth::AuthConfig;

/// Binary-specific configuration (not part of the library).
pub(crate) struct BinaryConfig {
    /// Library server config.
    pub server: ServerConfig,
    /// Control-API authentication (disabled with no secret by default).
    pub auth: AuthConfig,
    /// TCP bind address for JSON-RPC control. Default: 0.0.0.0.
    pub control_bind_address: String,
    /// TCP port for JSON-RPC control. Default: 1705.
    pub control_port: u16,
    /// TCP bind address for HTTP JSON-RPC + Snapweb. Default: 0.0.0.0.
    pub http_bind_address: String,
    /// HTTP port for JSON-RPC + Snapweb. Default: 1780.
    pub http_port: u16,
    /// TCP bind address for the binary audio protocol. Default: 0.0.0.0.
    pub stream_bind_address: String,
    /// TCP port for the binary audio protocol. Default: 1704.
    pub stream_port: u16,
    /// Path to Snapweb static files (None = disabled).
    pub doc_root: Option<String>,
    /// Stream source URIs.
    pub sources: Vec<String>,
}

impl Default for BinaryConfig {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            auth: AuthConfig::default(),
            control_bind_address: snapcast_proto::DEFAULT_BIND_ADDRESS.into(),
            control_port: snapcast_proto::DEFAULT_CONTROL_PORT,
            http_bind_address: snapcast_proto::DEFAULT_BIND_ADDRESS.into(),
            http_port: snapcast_proto::DEFAULT_HTTP_PORT,
            stream_bind_address: snapcast_proto::DEFAULT_BIND_ADDRESS.into(),
            stream_port: snapcast_proto::DEFAULT_STREAM_PORT,
            doc_root: None,
            sources: default_sources(),
        }
    }
}

fn default_sources() -> Vec<String> {
    #[cfg(unix)]
    {
        vec!["pipe:///tmp/snapfifo?name=default".into()]
    }
    #[cfg(not(unix))]
    {
        vec![format!(
            "tcp://{}:4953?name=default",
            snapcast_proto::DEFAULT_BIND_ADDRESS
        )]
    }
}

/// Parse a snapserver.conf INI file into a [`BinaryConfig`].
pub(crate) fn parse_config_file(path: &str) -> BinaryConfig {
    let mut config = BinaryConfig::default();

    let ini = match Ini::load_from_file(path) {
        Ok(ini) => ini,
        Err(e) => {
            tracing::debug!(path, error = %e, "Config file not found, using defaults");
            return config;
        }
    };

    tracing::info!(path, "Loaded config file");

    if let Some(s) = ini.section(Some("http")) {
        get_u16(s, "port", |v| config.http_port = v);
        get_bind_address(s, |v| config.http_bind_address = v.to_string());
        get_str(s, "doc_root", |v| config.doc_root = Some(v.to_string()));
    }

    if let Some(s) = ini.section(Some("tcp-control")) {
        get_u16(s, "port", |v| config.control_port = v);
        get_bind_address(s, |v| config.control_bind_address = v.to_string());
    }

    if let Some(s) = ini.section(Some("auth")) {
        get_bool(s, "enabled", |v| config.auth.enabled = v);
        get_str(s, "secret", |v| config.auth.secret = v.to_string());
    }

    if let Some(s) = ini.section(Some("tcp-streaming")) {
        get_u16(s, "port", |v| config.stream_port = v);
        get_bind_address(s, |v| config.stream_bind_address = v.to_string());
    }

    if let Some(s) = ini.section(Some("stream")) {
        let sources: Vec<String> = s.get_all("source").map(String::from).collect();
        if !sources.is_empty() {
            config.sources = sources;
        }
        get_str(s, "codec", |v| config.server.codec = v.to_string());
        get_str(s, "sampleformat", |v| {
            config.server.sample_format = v.to_string();
        });
        get_u32(s, "buffer", |v| config.server.buffer_ms = v);
    }

    config
}

fn get_str<F: FnOnce(&str)>(section: &ini::Properties, key: &str, f: F) {
    if let Some(v) = section.get(key) {
        f(v);
    }
}

fn get_u16<F: FnOnce(u16)>(section: &ini::Properties, key: &str, f: F) {
    if let Some(v) = section.get(key).and_then(|v| v.parse().ok()) {
        f(v);
    }
}

fn get_u32<F: FnOnce(u32)>(section: &ini::Properties, key: &str, f: F) {
    if let Some(v) = section.get(key).and_then(|v| v.parse().ok()) {
        f(v);
    }
}

fn get_bool<F: FnOnce(bool)>(section: &ini::Properties, key: &str, f: F) {
    if let Some(v) = section.get(key) {
        match v.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" | "1" => f(true),
            "false" | "no" | "off" | "0" => f(false),
            _ => {}
        }
    }
}

fn get_bind_address<F: FnMut(&str)>(section: &ini::Properties, mut f: F) {
    if let Some(v) = section
        .get("bind_to_address")
        .or_else(|| section.get("bind_address"))
    {
        f(v);
    }
}

/// CLI overrides.
pub(crate) struct CliOverrides {
    pub stream_bind_address: Option<String>,
    pub stream_port: Option<u16>,
    pub control_bind_address: Option<String>,
    pub control_port: Option<u16>,
    pub http_bind_address: Option<String>,
    pub http_port: Option<u16>,
    pub doc_root: Option<String>,
    pub buffer: Option<u32>,
    pub codec: Option<String>,
    pub sampleformat: Option<String>,
    pub sources: Vec<String>,
    /// Enable control-API auth (CLI can only turn it on; config sets both).
    pub auth_enabled: bool,
    /// Override the auth secret.
    pub auth_secret: Option<String>,
    #[cfg(feature = "mdns")]
    pub no_mdns: bool,
    #[cfg(feature = "mdns")]
    pub mdns_name: Option<String>,
}

/// Merge CLI overrides into config.
pub(crate) fn merge_cli(mut config: BinaryConfig, cli: CliOverrides) -> BinaryConfig {
    if let Some(v) = cli.stream_port {
        config.stream_port = v;
    }
    if let Some(v) = cli.stream_bind_address {
        config.stream_bind_address = v;
    }
    if let Some(v) = cli.control_port {
        config.control_port = v;
    }
    if let Some(v) = cli.control_bind_address {
        config.control_bind_address = v;
    }
    if let Some(v) = cli.http_port {
        config.http_port = v;
    }
    if let Some(v) = cli.http_bind_address {
        config.http_bind_address = v;
    }
    if let Some(v) = cli.doc_root {
        config.doc_root = Some(v);
    }
    if let Some(v) = cli.buffer {
        config.server.buffer_ms = v;
    }
    if let Some(v) = cli.codec {
        config.server.codec = v;
    }
    if let Some(v) = cli.sampleformat {
        config.server.sample_format = v;
    }
    if !cli.sources.is_empty() {
        config.sources = cli.sources;
    }
    if cli.auth_enabled {
        config.auth.enabled = true;
    }
    if let Some(v) = cli.auth_secret {
        config.auth.secret = v;
    }
    #[cfg(feature = "mdns")]
    let _ = (cli.no_mdns, cli.mdns_name); // handled in main.rs

    config
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parse_minimal_config() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            tmp,
            "[stream]\nsource = pipe:///tmp/snapfifo?name=test\n\n[http]\nbind_to_address = ::1\nport = 8080\ndoc_root = /var/www\n\n[tcp-control]\nbind_address = 127.0.0.1\n\n[tcp-streaming]\nbind_to_address = 127.0.0.1\nport = 2704"
        )
        .unwrap();

        let config = parse_config_file(tmp.path().to_str().unwrap());
        assert_eq!(config.sources, vec!["pipe:///tmp/snapfifo?name=test"]);
        assert_eq!(config.http_bind_address, "::1");
        assert_eq!(config.http_port, 8080);
        assert_eq!(config.doc_root, Some("/var/www".into()));
        assert_eq!(config.control_bind_address, "127.0.0.1");
        assert_eq!(config.stream_bind_address, "127.0.0.1");
        assert_eq!(config.stream_port, 2704);
    }

    #[test]
    fn missing_file_returns_defaults() {
        let config = parse_config_file("/nonexistent/snapserver.conf");
        assert_eq!(config.stream_port, 1704);
        // No built-in secret, auth off by default.
        assert!(!config.auth.enabled);
        assert!(config.auth.secret.is_empty());
    }

    #[test]
    fn parse_auth_section() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "[auth]\nenabled = true\nsecret = my-strong-secret").unwrap();
        let config = parse_config_file(tmp.path().to_str().unwrap());
        assert!(config.auth.enabled);
        assert_eq!(config.auth.secret, "my-strong-secret");
        assert!(config.auth.validate().is_ok());
    }

    #[test]
    fn merge_cli_overrides() {
        let config = BinaryConfig::default();
        let merged = merge_cli(
            config,
            CliOverrides {
                stream_bind_address: Some("::1".into()),
                stream_port: Some(9704),
                control_bind_address: None,
                control_port: None,
                http_bind_address: None,
                http_port: None,
                doc_root: None,
                buffer: None,
                codec: None,
                sampleformat: None,
                sources: vec![],
                auth_enabled: false,
                auth_secret: None,
                #[cfg(feature = "mdns")]
                no_mdns: false,
                #[cfg(feature = "mdns")]
                mdns_name: None,
            },
        );
        assert_eq!(merged.stream_bind_address, "::1");
        assert_eq!(merged.stream_port, 9704);
        assert_eq!(merged.control_port, 1705);
    }

    // ---- Wave-4 additions ----

    /// Build `CliOverrides` with all fields at their no-op (None/empty/false)
    /// default so individual tests only set the one field under test.
    fn empty_cli() -> CliOverrides {
        CliOverrides {
            stream_bind_address: None,
            stream_port: None,
            control_bind_address: None,
            control_port: None,
            http_bind_address: None,
            http_port: None,
            doc_root: None,
            buffer: None,
            codec: None,
            sampleformat: None,
            sources: vec![],
            auth_enabled: false,
            auth_secret: None,
            #[cfg(feature = "mdns")]
            no_mdns: false,
            #[cfg(feature = "mdns")]
            mdns_name: None,
        }
    }

    fn config_from(contents: &str) -> BinaryConfig {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "{contents}").unwrap();
        tmp.flush().unwrap();
        parse_config_file(tmp.path().to_str().unwrap())
    }

    // ---- Defaults ----

    #[test]
    fn default_config_has_expected_ports_and_addresses() {
        let config = BinaryConfig::default();
        assert_eq!(config.control_port, 1705);
        assert_eq!(config.http_port, 1780);
        assert_eq!(config.stream_port, 1704);
        assert_eq!(config.control_bind_address, "0.0.0.0");
        assert_eq!(config.http_bind_address, "0.0.0.0");
        assert_eq!(config.stream_bind_address, "0.0.0.0");
        assert_eq!(config.doc_root, None);
        // Feature-dependent default codec: flac > pcm.
        // With default features, `flac` is enabled.
        assert_eq!(config.server.codec, "flac");
        assert_eq!(config.server.sample_format, "48000:16:2");
        assert_eq!(config.server.buffer_ms, 1000);
    }

    #[test]
    fn default_sources_is_non_empty_single_entry() {
        let sources = default_sources();
        assert_eq!(sources.len(), 1);
        // On unix the default source is the snapfifo pipe.
        #[cfg(unix)]
        assert_eq!(sources[0], "pipe:///tmp/snapfifo?name=default");
        #[cfg(not(unix))]
        assert!(sources[0].starts_with("tcp://"));
    }

    #[test]
    fn missing_file_keeps_default_sources() {
        let config = parse_config_file("/nonexistent/does/not/exist.conf");
        assert_eq!(config.sources, default_sources());
        assert_eq!(config.http_port, 1780);
        assert_eq!(config.control_port, 1705);
        assert_eq!(config.server.codec, "flac");
    }

    // ---- Multiple sources ----

    #[test]
    fn multiple_sources_are_collected_in_order() {
        let config =
            config_from("[stream]\nsource = pipe:///tmp/a?name=a\nsource = pipe:///tmp/b?name=b\n");
        assert_eq!(
            config.sources,
            vec!["pipe:///tmp/a?name=a", "pipe:///tmp/b?name=b"]
        );
    }

    #[test]
    fn empty_stream_section_leaves_default_sources() {
        // A [stream] section with no `source` keys must not clobber the
        // defaults with an empty Vec.
        let config = config_from("[stream]\ncodec = pcm\n");
        assert_eq!(config.sources, default_sources());
        assert_eq!(config.server.codec, "pcm");
    }

    // ---- Codec / sample format / buffer overrides from INI ----

    #[test]
    fn stream_section_overrides_codec_sampleformat_buffer() {
        let config =
            config_from("[stream]\ncodec = pcm\nsampleformat = 44100:24:2\nbuffer = 2000\n");
        assert_eq!(config.server.codec, "pcm");
        assert_eq!(config.server.sample_format, "44100:24:2");
        assert_eq!(config.server.buffer_ms, 2000);
    }

    #[test]
    fn invalid_buffer_value_leaves_default() {
        // Non-numeric buffer must be ignored (parse fails silently).
        let config = config_from("[stream]\nbuffer = notanumber\n");
        assert_eq!(config.server.buffer_ms, 1000);
    }

    // ---- Port parsing edge cases ----

    #[test]
    fn port_at_u16_max_is_parsed() {
        let config = config_from("[http]\nport = 65535\n");
        assert_eq!(config.http_port, 65535);
    }

    #[test]
    fn port_above_u16_max_is_ignored() {
        // 99999 > u16::MAX (65535) — parse fails, default retained.
        let config = config_from("[http]\nport = 99999\n");
        assert_eq!(config.http_port, 1780);
    }

    #[test]
    fn non_numeric_port_is_ignored() {
        let config = config_from("[tcp-control]\nport = abc\n");
        assert_eq!(config.control_port, 1705);
    }

    #[test]
    fn negative_port_is_ignored() {
        // "-1" does not parse as u16.
        let config = config_from("[tcp-streaming]\nport = -1\n");
        assert_eq!(config.stream_port, 1704);
    }

    #[test]
    fn port_zero_is_parsed() {
        let config = config_from("[tcp-streaming]\nport = 0\n");
        assert_eq!(config.stream_port, 0);
    }

    // ---- bind address precedence ----

    #[test]
    fn bind_to_address_takes_precedence_over_bind_address() {
        let config =
            config_from("[http]\nbind_to_address = 10.0.0.1\nbind_address = 192.168.1.1\n");
        assert_eq!(config.http_bind_address, "10.0.0.1");
    }

    #[test]
    fn bind_address_used_when_bind_to_address_absent() {
        let config = config_from("[tcp-control]\nbind_address = 127.0.0.1\n");
        assert_eq!(config.control_bind_address, "127.0.0.1");
    }

    #[test]
    fn missing_bind_keys_leave_default() {
        let config = config_from("[http]\nport = 1234\n");
        assert_eq!(config.http_bind_address, "0.0.0.0");
        assert_eq!(config.http_port, 1234);
    }

    // ---- auth section / bool parsing ----

    #[test]
    fn auth_enabled_accepts_various_truthy_values() {
        for truthy in ["true", "TRUE", "Yes", "on", "ON", "1"] {
            let config = config_from(&format!("[auth]\nenabled = {truthy}\nsecret = s\n"));
            assert!(config.auth.enabled, "value {truthy:?} should enable auth");
        }
    }

    #[test]
    fn auth_enabled_accepts_various_falsy_values() {
        for falsy in ["false", "FALSE", "No", "off", "OFF", "0"] {
            // Start from an enabled default via CLI would be cleaner, but the
            // default is already disabled, so assert the value is not flipped on.
            let config = config_from(&format!("[auth]\nenabled = {falsy}\n"));
            assert!(
                !config.auth.enabled,
                "value {falsy:?} should not enable auth"
            );
        }
    }

    #[test]
    fn auth_enabled_ignores_unrecognized_value() {
        // A garbage value leaves `enabled` at its default (false) — the closure
        // is never called for unrecognized strings.
        let config = config_from("[auth]\nenabled = maybe\n");
        assert!(!config.auth.enabled);
    }

    #[test]
    fn auth_secret_with_surrounding_whitespace_bool() {
        // get_bool trims whitespace before matching.
        let config = config_from("[auth]\nenabled =   true  \n");
        assert!(config.auth.enabled);
    }

    #[test]
    fn auth_enabled_without_secret_fails_validation() {
        let config = config_from("[auth]\nenabled = true\n");
        assert!(config.auth.enabled);
        assert!(config.auth.secret.is_empty());
        // Enabled auth with no secret is an invalid combination.
        assert!(config.auth.validate().is_err());
    }

    // ---- merge_cli: overrides, no-ops, precedence ----

    #[test]
    fn merge_cli_none_fields_leave_config_unchanged() {
        let config = config_from(
            "[http]\nport = 8080\nbind_to_address = ::1\n[stream]\ncodec = pcm\nbuffer = 500\n",
        );
        let merged = merge_cli(config, empty_cli());
        assert_eq!(merged.http_port, 8080);
        assert_eq!(merged.http_bind_address, "::1");
        assert_eq!(merged.server.codec, "pcm");
        assert_eq!(merged.server.buffer_ms, 500);
    }

    #[test]
    fn merge_cli_overrides_all_scalar_fields() {
        let merged = merge_cli(
            BinaryConfig::default(),
            CliOverrides {
                stream_bind_address: Some("1.1.1.1".into()),
                stream_port: Some(2001),
                control_bind_address: Some("2.2.2.2".into()),
                control_port: Some(2002),
                http_bind_address: Some("3.3.3.3".into()),
                http_port: Some(2003),
                doc_root: Some("/srv/web".into()),
                buffer: Some(3000),
                codec: Some("pcm".into()),
                sampleformat: Some("96000:24:2".into()),
                ..empty_cli()
            },
        );
        assert_eq!(merged.stream_bind_address, "1.1.1.1");
        assert_eq!(merged.stream_port, 2001);
        assert_eq!(merged.control_bind_address, "2.2.2.2");
        assert_eq!(merged.control_port, 2002);
        assert_eq!(merged.http_bind_address, "3.3.3.3");
        assert_eq!(merged.http_port, 2003);
        assert_eq!(merged.doc_root, Some("/srv/web".into()));
        assert_eq!(merged.server.buffer_ms, 3000);
        assert_eq!(merged.server.codec, "pcm");
        assert_eq!(merged.server.sample_format, "96000:24:2");
    }

    #[test]
    fn merge_cli_sources_override_only_when_non_empty() {
        // Non-empty CLI sources replace config sources.
        let config = config_from("[stream]\nsource = pipe:///tmp/from-file\n");
        let merged = merge_cli(
            config,
            CliOverrides {
                sources: vec!["tcp://cli:4953".into()],
                ..empty_cli()
            },
        );
        assert_eq!(merged.sources, vec!["tcp://cli:4953"]);

        // Empty CLI sources leave the file-provided sources intact.
        let config2 = config_from("[stream]\nsource = pipe:///tmp/from-file\n");
        let merged2 = merge_cli(config2, empty_cli());
        assert_eq!(merged2.sources, vec!["pipe:///tmp/from-file"]);
    }

    #[test]
    fn merge_cli_auth_enabled_only_turns_on() {
        let mut config = BinaryConfig::default();
        config.auth.enabled = false;
        let merged = merge_cli(
            config,
            CliOverrides {
                auth_enabled: true,
                ..empty_cli()
            },
        );
        assert!(merged.auth.enabled);
    }

    #[test]
    fn merge_cli_auth_enabled_false_does_not_disable_config() {
        // CLI can only turn auth ON; a false flag must not turn off auth that
        // the config file enabled.
        let mut config = BinaryConfig::default();
        config.auth.enabled = true;
        let merged = merge_cli(
            config,
            CliOverrides {
                auth_enabled: false,
                ..empty_cli()
            },
        );
        assert!(merged.auth.enabled);
    }

    #[test]
    fn merge_cli_auth_secret_overrides() {
        let merged = merge_cli(
            BinaryConfig::default(),
            CliOverrides {
                auth_secret: Some("cli-secret".into()),
                ..empty_cli()
            },
        );
        assert_eq!(merged.auth.secret, "cli-secret");
    }

    // ---- combined config + CLI precedence ----

    #[test]
    fn cli_overrides_take_precedence_over_config_file() {
        let config =
            config_from("[http]\nport = 8080\n[tcp-control]\nport = 9705\n[stream]\ncodec = pcm\n");
        let merged = merge_cli(
            config,
            CliOverrides {
                http_port: Some(1111),
                codec: Some("flac".into()),
                ..empty_cli()
            },
        );
        // Overridden by CLI.
        assert_eq!(merged.http_port, 1111);
        assert_eq!(merged.server.codec, "flac");
        // Not overridden by CLI — keeps the config-file value.
        assert_eq!(merged.control_port, 9705);
    }

    // ---- INI structure edge cases ----

    #[test]
    fn unknown_sections_and_keys_are_ignored() {
        let config =
            config_from("[unknown-section]\nfoo = bar\n[http]\nport = 4242\nunknown_key = zzz\n");
        assert_eq!(config.http_port, 4242);
        // Everything else stays at defaults.
        assert_eq!(config.control_port, 1705);
    }

    #[test]
    fn empty_file_yields_defaults() {
        let config = config_from("");
        assert_eq!(config.http_port, 1780);
        assert_eq!(config.control_port, 1705);
        assert_eq!(config.stream_port, 1704);
        assert_eq!(config.sources, default_sources());
    }
}
