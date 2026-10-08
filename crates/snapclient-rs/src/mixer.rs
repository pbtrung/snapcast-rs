//! Volume mixer — software (PCM scaling) or hardware (ALSA control).

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use snapcast_client::config::{MixerMode, MixerSettings};

/// Shared linear gain applied by the player (software mixer).
pub struct VolumeState {
    /// `f32` gain as bits, so the audio callback can read it lock-free.
    gain_bits: AtomicU32,
}

impl VolumeState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            gain_bits: AtomicU32::new(1.0f32.to_bits()),
        })
    }

    /// Get the linear gain factor (0.0–1.0).
    pub fn gain(&self) -> f32 {
        f32::from_bits(self.gain_bits.load(Ordering::Relaxed))
    }

    fn set_gain(&self, gain: f32) {
        self.gain_bits.store(gain.to_bits(), Ordering::Relaxed);
    }
}

/// Software volume curve, as C++ snapclient's `software[:poly|exp[:<param>]]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VolumeCurve {
    /// `gain = v^exponent` (default exponent 3).
    Poly(f64),
    /// `gain = (base^v - 1) / (base - 1)` (default base 10; the default curve).
    Exp(f64),
}

impl VolumeCurve {
    /// Parse the software mixer parameter: `[poly|exp][:<param>]`.
    fn parse(param: &str) -> Self {
        let (mode, value) = param.split_once(':').unwrap_or((param, ""));
        let value = match value.parse::<f64>() {
            Ok(v) if v > 0.0 && v.is_finite() => Some(v),
            _ if value.is_empty() => None,
            _ => {
                tracing::warn!(value, "Invalid software mixer parameter, using default");
                None
            }
        };
        match mode {
            "poly" => Self::Poly(value.unwrap_or(3.0)),
            "exp" | "" => Self::Exp(value.filter(|&b| b != 1.0).unwrap_or(10.0)),
            other => {
                tracing::warn!(mode = other, "Unknown software mixer curve, using exp");
                Self::Exp(10.0)
            }
        }
    }

    /// Linear gain for a volume percentage.
    fn gain(self, percent: u8) -> f32 {
        let v = f64::from(percent.min(100)) / 100.0;
        let gain = match self {
            Self::Poly(exponent) => v.powf(exponent),
            Self::Exp(base) => (base.powf(v) - 1.0) / (base - 1.0),
        };
        gain as f32
    }
}

/// Mixer backend.
pub enum Mixer {
    /// PCM amplitude scaling (default).
    Software {
        volume: Arc<VolumeState>,
        curve: VolumeCurve,
    },
    /// ALSA hardware mixer control (Linux only).
    #[cfg(target_os = "linux")]
    Hardware { control: String },
    /// No volume control.
    None,
}

impl Mixer {
    /// Build the mixer from CLI settings. Returns it with the gain handle the
    /// player applies (stays at unity unless the mixer is software).
    pub fn new(settings: &MixerSettings) -> (Self, Arc<VolumeState>) {
        let volume = VolumeState::new();
        let param = settings.parameter.as_str();
        let mixer = match settings.mode {
            MixerMode::Software => Mixer::Software {
                volume: volume.clone(),
                curve: VolumeCurve::parse(param),
            },
            #[cfg(target_os = "linux")]
            MixerMode::Hardware => {
                let control = if param.is_empty() {
                    detect_alsa_control().unwrap_or_else(|| "Master".to_string())
                } else {
                    param.to_string()
                };
                if !validate_alsa_control(&control) {
                    tracing::warn!(
                        control,
                        available = list_alsa_controls().as_deref().unwrap_or("none"),
                        "ALSA mixer control not found"
                    );
                } else {
                    tracing::info!(control, "Hardware mixer initialized");
                }
                Mixer::Hardware { control }
            }
            #[cfg(not(target_os = "linux"))]
            MixerMode::Hardware => {
                tracing::warn!("Hardware mixer not supported on this platform, using software");
                Mixer::Software {
                    volume: volume.clone(),
                    curve: VolumeCurve::parse(""),
                }
            }
            MixerMode::Script => {
                tracing::warn!("Script mixer not implemented, using software");
                Mixer::Software {
                    volume: volume.clone(),
                    curve: VolumeCurve::parse(""),
                }
            }
            MixerMode::None => Mixer::None,
        };
        (mixer, volume)
    }

    /// Apply a volume change from the server.
    pub fn set_volume(&self, percent: u8, muted: bool) {
        match self {
            Mixer::Software { volume, curve } => {
                volume.set_gain(if muted { 0.0 } else { curve.gain(percent) });
            }
            #[cfg(target_os = "linux")]
            Mixer::Hardware { control } => {
                set_alsa_volume(control, percent, muted);
            }
            Mixer::None => {}
        }
    }
}

#[cfg(target_os = "linux")]
fn set_alsa_volume(control: &str, percent: u8, muted: bool) {
    let vol = if muted { 0 } else { percent };
    if let Err(e) = set_alsa_volume_inner(control, vol) {
        tracing::warn!(control, error = %e, "Failed to set ALSA volume");
    } else {
        tracing::debug!(control, percent, muted, "Hardware volume set");
    }
}

#[cfg(target_os = "linux")]
fn set_alsa_volume_inner(control: &str, percent: u8) -> anyhow::Result<()> {
    use alsa::mixer::{Mixer, SelemId};
    let mixer = Mixer::new("default", false)?;
    let selem_id = SelemId::new(control, 0);
    let selem = mixer
        .find_selem(&selem_id)
        .ok_or_else(|| anyhow::anyhow!("ALSA control '{control}' not found"))?;
    let (min, max) = selem.get_playback_volume_range();
    // Perceptual volume curve (cubic) — closer to perceived loudness than linear.
    let normalized = f64::from(percent) / 100.0;
    let curved = normalized * normalized * normalized;
    let vol = min + ((max - min) as f64 * curved) as i64;
    selem.set_playback_volume_all(vol)?;
    if selem.has_playback_switch() {
        selem.set_playback_switch_all(if percent == 0 { 0 } else { 1 })?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn validate_alsa_control(control: &str) -> bool {
    use alsa::mixer::{Mixer, SelemId};
    let Ok(mixer) = Mixer::new("default", false) else {
        return false;
    };
    mixer.find_selem(&SelemId::new(control, 0)).is_some()
}

#[cfg(target_os = "linux")]
fn list_alsa_controls() -> Option<String> {
    use alsa::mixer::{Mixer, Selem};
    let mixer = Mixer::new("default", false).ok()?;
    let names: Vec<String> = mixer
        .iter()
        .filter_map(|elem| {
            let selem = Selem::new(elem)?;
            Some(selem.get_id().get_name().ok()?.to_string())
        })
        .collect();
    Some(names.join(", "))
}

#[cfg(target_os = "linux")]
fn detect_alsa_control() -> Option<String> {
    for candidate in ["Master", "Digital", "PCM", "Speaker"] {
        if validate_alsa_control(candidate) {
            return Some(candidate.to_string());
        }
    }
    use alsa::mixer::{Mixer, Selem};
    let mixer = Mixer::new("default", false).ok()?;
    mixer.iter().find_map(|elem| {
        let selem = Selem::new(elem)?;
        if selem.has_playback_volume() {
            Some(selem.get_id().get_name().ok()?.to_string())
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mixer(mode: MixerMode, parameter: &str) -> (Mixer, Arc<VolumeState>) {
        Mixer::new(&MixerSettings {
            mode,
            parameter: parameter.into(),
        })
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-6
    }

    // ---- VolumeState ----

    #[test]
    fn gain_default_is_unity() {
        assert_eq!(VolumeState::new().gain(), 1.0);
    }

    // ---- VolumeCurve ----

    #[test]
    fn default_curve_is_exp_base_10() {
        let curve = VolumeCurve::parse("");
        assert_eq!(curve, VolumeCurve::Exp(10.0));
        assert_eq!(curve.gain(0), 0.0);
        assert!(close(curve.gain(100), 1.0));
        // (10^0.5 - 1) / 9 ≈ 0.2403, as C++ snapclient
        assert!(close(curve.gain(50), ((10f64.sqrt() - 1.0) / 9.0) as f32));
    }

    #[test]
    fn poly_curve_and_params() {
        assert_eq!(VolumeCurve::parse("poly"), VolumeCurve::Poly(3.0));
        assert_eq!(VolumeCurve::parse("poly:2"), VolumeCurve::Poly(2.0));
        assert_eq!(VolumeCurve::parse("exp:20"), VolumeCurve::Exp(20.0));
        assert!(close(VolumeCurve::Poly(2.0).gain(50), 0.25));
        assert!(close(VolumeCurve::Poly(1.0).gain(30), 0.3));
    }

    #[test]
    fn invalid_curve_params_fall_back_to_defaults() {
        assert_eq!(VolumeCurve::parse("poly:-1"), VolumeCurve::Poly(3.0));
        assert_eq!(VolumeCurve::parse("poly:abc"), VolumeCurve::Poly(3.0));
        // base 1 would divide by zero
        assert_eq!(VolumeCurve::parse("exp:1"), VolumeCurve::Exp(10.0));
        assert_eq!(VolumeCurve::parse("bogus"), VolumeCurve::Exp(10.0));
    }

    #[test]
    fn curves_are_monotonic() {
        for curve in [VolumeCurve::Exp(10.0), VolumeCurve::Poly(3.0)] {
            for p in 0u8..100 {
                assert!(curve.gain(p) < curve.gain(p + 1), "{curve:?} at {p}");
            }
        }
    }

    // ---- Mixer::new ----

    #[test]
    fn software_handle_is_shared_with_backend() {
        let (mixer, vol) = mixer(MixerMode::Software, "");
        match &mixer {
            Mixer::Software { volume, .. } => assert!(Arc::ptr_eq(volume, &vol)),
            _ => panic!("expected software backend"),
        }
    }

    #[test]
    fn script_falls_back_to_software() {
        let (mixer, _) = mixer(MixerMode::Script, "");
        assert!(matches!(mixer, Mixer::Software { .. }));
    }

    #[test]
    #[cfg(not(target_os = "linux"))]
    fn hardware_falls_back_to_software_off_linux() {
        let (mixer, _) = mixer(MixerMode::Hardware, "Master");
        assert!(matches!(mixer, Mixer::Software { .. }));
    }

    // ---- Mixer::set_volume ----

    #[test]
    fn set_volume_software_applies_curve() {
        let (mixer, vol) = mixer(MixerMode::Software, "poly:1");
        mixer.set_volume(75, false);
        assert!(close(vol.gain(), 0.75));
        mixer.set_volume(100, false);
        assert!(close(vol.gain(), 1.0));
        mixer.set_volume(0, false);
        assert_eq!(vol.gain(), 0.0);
    }

    #[test]
    fn set_volume_software_mute_overrides_percent() {
        let (mixer, vol) = mixer(MixerMode::Software, "");
        mixer.set_volume(80, true);
        assert_eq!(vol.gain(), 0.0);
        mixer.set_volume(80, false);
        assert!(vol.gain() > 0.0);
    }

    #[test]
    fn set_volume_none_leaves_unity_gain() {
        let (mixer, vol) = mixer(MixerMode::None, "");
        mixer.set_volume(10, true);
        assert_eq!(vol.gain(), 1.0);
    }
}
