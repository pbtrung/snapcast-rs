//! CLI argument parsing — maps command-line args to [`ClientSettings`].

use anyhow::{Context, Result, bail};
use clap::Parser;

use snapcast_client::config::{self, Auth, ClientSettings, MixerMode, ServerSettings};

/// Snapcast client — synchronized multiroom audio player.
#[derive(Parser, Debug)]
#[command(
    version,
    about,
    after_help = "\
  With 'url' = tcp://<snapserver host or IP>[:port]\n\
  For example: 'tcp://192.168.1.1:1704', or 'tcp://[::1]:1704'"
)]
pub struct Cli {
    /// Snapserver URL (required): `tcp://<host>[:<port>]` (default port 1704) or `ws://<host>[:<port>]` (default port 1780)
    pub url: Option<String>,

    /// Instance id when running multiple instances on the same host
    #[arg(short, long, default_value_t = 1)]
    pub instance: u32,

    /// Unique host id (default: MAC address)
    #[arg(long = "hostID", default_value = "")]
    pub host_id: String,

    /// List PCM devices (macOS only)
    #[arg(short, long)]
    pub list: bool,

    /// PCM device index or name (accepted for compatibility; the default output device is used)
    #[arg(short, long, default_value = "default")]
    pub soundcard: String,

    /// Additional latency of the audio device (ms)
    #[arg(long, default_value_t = 0)]
    pub latency: i32,

    /// Resample to `<rate>:<bits>:<channels>` (accepted for compatibility; not applied)
    #[arg(long)]
    pub sampleformat: Option<String>,

    /// Audio player backend and optional parameters: `<name>[:<params>]` (accepted for compatibility; output always uses cpal)
    #[arg(long, default_value = "")]
    pub player: String,

    /// Mixer: `software[:poly|exp[:<param>]]`, `hardware[:<control>]`, `script` (falls back to software) or `none`
    #[arg(long, default_value = "software")]
    pub mixer: String,

    /// Daemonize, optional process priority [-20..19]
    #[cfg(unix)]
    #[arg(short, long)]
    pub daemon: Option<Option<i32>>,

    /// The `user[:group]` to run snapclient as when daemonized (not implemented yet)
    #[cfg(unix)]
    #[arg(long)]
    pub user: Option<String>,

    /// Log sink: null|system|stdout|stderr|file:`<path>` (`system` logs to stderr)
    #[arg(long, default_value = "stdout")]
    pub logsink: String,

    /// Log filter: `<tag>:<level>[,<tag>:<level>]*`
    #[arg(long, default_value = "*:info")]
    pub logfilter: String,
}

impl Cli {
    /// Parse CLI args and build a [`ClientSettings`].
    pub fn into_settings(self) -> Result<ClientSettings> {
        let Some(url) = self.url.as_deref() else {
            bail!("no server URL given, e.g. tcp://192.168.1.1:1704");
        };
        let server = parse_url(url)?;

        // Player
        let (player_name, player_param) = if self.player.is_empty() {
            (String::new(), String::new())
        } else if let Some((name, param)) = self.player.split_once(':') {
            (name.to_string(), param.to_string())
        } else {
            (self.player, String::new())
        };

        // Sample format
        let sample_format = match self.sampleformat {
            Some(ref sf) => sf
                .parse()
                .with_context(|| format!("invalid sample format: {sf}"))?,
            None => snapcast_client::SampleFormat::default(),
        };

        // Mixer
        let (mixer_mode_str, mixer_param) = self
            .mixer
            .split_once(':')
            .map(|(m, p)| (m, p.to_string()))
            .unwrap_or((&self.mixer, String::new()));
        let mixer_mode = match mixer_mode_str {
            "software" => MixerMode::Software,
            "hardware" => MixerMode::Hardware,
            "script" => MixerMode::Script,
            "none" => MixerMode::None,
            other => bail!("unknown mixer mode: {other}"),
        };

        Ok(ClientSettings {
            instance: self.instance,
            host_id: self.host_id,
            server,
            player: config::PlayerSettings {
                player_name,
                parameter: player_param,
                latency: self.latency,
                pcm_device: config::PcmDevice {
                    name: self.soundcard,
                    ..Default::default()
                },
                sample_format,
                mixer: config::MixerSettings {
                    mode: mixer_mode,
                    parameter: mixer_param,
                },
            },
            logging: config::LoggingSettings {
                sink: self.logsink,
                filter: self.logfilter,
            },
            #[cfg(unix)]
            daemon: self.daemon.map(|priority| config::DaemonSettings {
                priority: priority.or(Some(-3)),
                user: self.user,
            }),
        })
    }
}

/// Parse a snapcast URL into [`ServerSettings`].
fn parse_url(url: &str) -> Result<ServerSettings> {
    let mut settings = ServerSettings::default();

    let (scheme, rest) = url
        .split_once("://")
        .with_context(|| format!("invalid URL, expected <scheme>://<host>[:port]: {url}"))?;

    let default_port = match scheme {
        snapcast_proto::SCHEME_TCP => snapcast_proto::DEFAULT_STREAM_PORT,
        // WebSocket streaming clients connect to the server's HTTP port.
        snapcast_proto::SCHEME_WS => snapcast_proto::DEFAULT_HTTP_PORT,
        _ => bail!("unsupported scheme: {scheme} (expected tcp or ws)"),
    };
    settings.scheme = scheme.to_string();

    // Extract optional user:password@
    let rest = if let Some((userinfo, host_part)) = rest.rsplit_once('@') {
        let (user, password) = userinfo
            .split_once(':')
            .context("invalid credentials, expected user:password@host")?;
        settings.auth = Some(Auth {
            scheme: "Basic".into(),
            param: base64_encode_credentials(user, password),
        });
        host_part
    } else {
        rest
    };

    let (host, port) = parse_host_port(rest, default_port)?;
    settings.host = host;
    settings.port = port;

    Ok(settings)
}

fn parse_host_port(input: &str, default_port: u16) -> Result<(String, u16)> {
    anyhow::ensure!(!input.is_empty(), "missing host");

    if let Some(stripped) = input.strip_prefix('[') {
        let (host, tail) = stripped
            .split_once(']')
            .context("invalid IPv6 host, missing closing ']'")?;
        anyhow::ensure!(!host.is_empty(), "missing IPv6 host");
        let port = if tail.is_empty() {
            default_port
        } else {
            let port_str = tail
                .strip_prefix(':')
                .with_context(|| format!("invalid IPv6 host suffix: {tail}"))?;
            parse_port(port_str)?
        };
        return Ok((host.to_string(), port));
    }

    if input.matches(':').count() == 1 {
        let (host, port_str) = input
            .rsplit_once(':')
            .expect("counted exactly one ':' before splitting");
        anyhow::ensure!(!host.is_empty(), "missing host");
        return Ok((host.to_string(), parse_port(port_str)?));
    }

    // No colons — plain hostname or IPv4
    if !input.contains(':') {
        return Ok((input.to_string(), default_port));
    }

    // Multiple colons without brackets — bare IPv6. Accept only if valid IPv6.
    // If the user intended a port, they must use bracket notation: [::1]:1704
    anyhow::ensure!(
        input.parse::<std::net::Ipv6Addr>().is_ok(),
        "ambiguous IPv6 address with port — use bracket notation: tcp://[{input}]:port"
    );
    Ok((input.to_string(), default_port))
}

fn parse_port(port_str: &str) -> Result<u16> {
    anyhow::ensure!(!port_str.is_empty(), "missing port");
    port_str
        .parse()
        .with_context(|| format!("invalid port: {port_str}"))
}

fn base64_encode_credentials(user: &str, password: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tcp_url() {
        let s = parse_url("tcp://192.168.1.1:1704").unwrap();
        assert_eq!(s.scheme, "tcp");
        assert_eq!(s.host, "192.168.1.1");
        assert_eq!(s.port, 1704);
        assert!(s.auth.is_none());
    }

    #[test]
    fn parse_ipv6_url() {
        let s = parse_url("tcp://[::1]:1704").unwrap();
        assert_eq!(s.scheme, "tcp");
        assert_eq!(s.host, "::1");
        assert_eq!(s.port, 1704);
    }

    #[test]
    fn parse_ipv6_url_default_port() {
        let s = parse_url("tcp://::1").unwrap();
        assert_eq!(s.host, "::1");
        assert_eq!(s.port, 1704);
    }

    #[test]
    fn parse_ipv6_ambiguous_with_port_rejected() {
        // "::1:99999" is not a valid IPv6 address, so it's rejected as ambiguous
        let err = parse_url("tcp://::1:99999").unwrap_err();
        assert!(err.to_string().contains("bracket notation"));
    }

    #[test]
    fn parse_url_with_credentials() {
        let s = parse_url("tcp://user:pass@myhost:1704").unwrap();
        assert_eq!(s.host, "myhost");
        let auth = s.auth.unwrap();
        assert_eq!(auth.scheme, "Basic");
        assert_eq!(auth.param, "dXNlcjpwYXNz");
    }

    #[test]
    fn parse_invalid_scheme() {
        assert!(parse_url("http://localhost").is_err());
    }

    #[test]
    fn parse_websocket_scheme() {
        let s = parse_url("ws://homeserver.local").unwrap();
        assert_eq!(s.scheme, "ws");
        assert_eq!(s.host, "homeserver.local");
        assert_eq!(s.port, snapcast_proto::DEFAULT_HTTP_PORT);
        let s = parse_url("ws://[::1]:8080").unwrap();
        assert_eq!((s.host.as_str(), s.port), ("::1", 8080));
        assert!(parse_url("wss://secure.host:1788").is_err());
    }

    #[test]
    fn parse_invalid_url() {
        assert!(parse_url("garbage").is_err());
    }

    #[test]
    fn url_is_required() {
        let cli = Cli::parse_from(["snapclient-rs"]);
        assert!(cli.url.is_none());
        assert!(cli.into_settings().is_err());
    }

    #[test]
    fn cli_into_settings_mixer() {
        let cli = Cli::parse_from(["snapclient-rs", "tcp://host", "--mixer", "hardware:hw:0"]);
        let s = cli.into_settings().unwrap();
        assert_eq!(s.player.mixer.mode, MixerMode::Hardware);
        assert_eq!(s.player.mixer.parameter, "hw:0");
    }

    #[test]
    fn cli_into_settings_player_with_params() {
        let cli = Cli::parse_from([
            "snapclient-rs",
            "--instance",
            "2",
            "--hostID",
            "my-id",
            "--player",
            "alsa:buffer_time=100",
            "--latency",
            "50",
            "--sampleformat",
            "48000:16:*",
            "--soundcard",
            "hw:1",
            "--logsink",
            "stderr",
            "--logfilter",
            "*:debug",
            "tcp://localhost:1704",
        ]);
        let s = cli.into_settings().unwrap();
        assert_eq!(s.instance, 2);
        assert_eq!(s.host_id, "my-id");
        assert_eq!(s.player.player_name, "alsa");
        assert_eq!(s.player.parameter, "buffer_time=100");
        assert_eq!(s.player.latency, 50);
        assert_eq!(s.player.sample_format.rate(), 48000);
        assert_eq!(s.player.sample_format.channels(), 0);
        assert_eq!(s.player.pcm_device.name, "hw:1");
    }

    #[test]
    fn cli_list_flag() {
        let cli = Cli::parse_from(["snapclient-rs", "--list"]);
        assert!(cli.list);
    }
}
