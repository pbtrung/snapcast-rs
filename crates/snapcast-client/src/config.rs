//! Client configuration types mirroring the C++ `ClientSettings`.

use snapcast_proto::SampleFormat;

/// Server connection settings.
#[derive(Debug, Clone)]
pub struct ServerSettings {
    /// Connection scheme: "tcp" or "ws".
    pub scheme: String,
    /// Server hostname or IP.
    pub host: String,
    /// Server port.
    pub port: u16,
    /// Optional authentication.
    pub auth: Option<Auth>,
}

/// Authentication credentials.
#[derive(Debug, Clone)]
pub struct Auth {
    /// Auth scheme (e.g. "Basic").
    pub scheme: String,
    /// Auth parameter (e.g. base64-encoded credentials).
    pub param: String,
}

/// Mixer mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MixerMode {
    /// Software volume control (default).
    #[default]
    Software,
    /// Hardware mixer (e.g. ALSA).
    Hardware,
    /// External script mixer.
    Script,
    /// No volume control.
    None,
}

/// Mixer settings.
#[derive(Debug, Clone, Default)]
pub struct MixerSettings {
    /// Mixer mode.
    pub mode: MixerMode,
    /// Mixer-specific parameter string.
    pub parameter: String,
}

/// PCM device identifier.
#[derive(Debug, Clone)]
pub struct PcmDevice {
    /// Device index (-1 for default).
    pub idx: i32,
    /// Device name.
    pub name: String,
    /// Human-readable description.
    pub description: String,
}

impl Default for PcmDevice {
    fn default() -> Self {
        Self {
            idx: -1,
            name: "default".into(),
            description: String::new(),
        }
    }
}

/// Audio player settings.
#[derive(Debug, Clone, Default)]
pub struct PlayerSettings {
    /// Backend name (e.g. "alsa", "coreaudio").
    pub player_name: String,
    /// Backend-specific parameters.
    pub parameter: String,
    /// Additional latency in milliseconds.
    pub latency: i32,
    /// PCM output device.
    pub pcm_device: PcmDevice,
    /// Requested sample format (default: server format).
    pub sample_format: SampleFormat,
    /// Volume mixer settings.
    pub mixer: MixerSettings,
}

/// Logging settings.
#[derive(Debug, Clone)]
pub struct LoggingSettings {
    /// Log sink: "stdout", "stderr", "null", "file:\<path\>".
    pub sink: String,
    /// Log filter: "\<tag\>:\<level\>[,...]".
    pub filter: String,
}

impl Default for LoggingSettings {
    fn default() -> Self {
        Self {
            sink: "stdout".into(),
            filter: "*:info".into(),
        }
    }
}

/// Daemon settings (Unix only).
#[cfg(unix)]
#[derive(Debug, Clone, Default)]
pub struct DaemonSettings {
    /// Process priority [-20..19], None if not daemonizing.
    pub priority: Option<i32>,
    /// `User[:group]` to run as.
    pub user: Option<String>,
}

/// Top-level client settings.
#[derive(Debug, Clone)]
pub struct ClientSettings {
    /// Instance id when running multiple clients on one host.
    pub instance: u32,
    /// Unique host identifier (default: MAC address).
    pub host_id: String,
    /// Server connection settings.
    pub server: ServerSettings,
    /// Audio player settings.
    pub player: PlayerSettings,
    /// Logging configuration.
    pub logging: LoggingSettings,
    /// Daemon settings (Unix only).
    #[cfg(unix)]
    pub daemon: Option<DaemonSettings>,
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            scheme: snapcast_proto::SCHEME_TCP.into(),
            host: String::new(),
            port: snapcast_proto::DEFAULT_STREAM_PORT,
            auth: None,
        }
    }
}

impl Default for ClientSettings {
    fn default() -> Self {
        Self {
            instance: 1,
            host_id: String::new(),
            server: ServerSettings::default(),
            player: PlayerSettings::default(),
            logging: LoggingSettings::default(),
            #[cfg(unix)]
            daemon: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_cpp() {
        let s = ClientSettings::default();
        assert_eq!(s.instance, 1);
        assert_eq!(s.server.scheme, "tcp");
        assert_eq!(s.server.port, 1704);
        assert_eq!(s.player.latency, 0);
        assert_eq!(s.player.pcm_device.name, "default");
        assert_eq!(s.player.mixer.mode, MixerMode::Software);
        assert_eq!(s.logging.filter, "*:info");
    }
}
