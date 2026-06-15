use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::RumbleConfig;

pub const CORE_SIMVARS: &[(&str, &str)] = &[
    ("AIRSPEED INDICATED", "Knots"),
    ("SIM ON GROUND", "Bool"),
    ("PLANE BANK DEGREES", "Degrees"),
    ("TRAILING EDGE FLAPS LEFT PERCENT", "Percent"),
    ("TRAILING EDGE FLAPS RIGHT PERCENT", "Percent"),
    ("FLAPS HANDLE INDEX", "Number"),
    ("GEAR HANDLE POSITION", "Bool"),
    ("STALL WARNING", "Bool"),
    ("ABSOLUTE TIME", "Seconds"),
    ("GROUND VELOCITY", "Knots"),
    ("PAUSED", "Bool"),
];

pub const CORE_SIMVAR_COUNT: usize = CORE_SIMVARS.len();

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PresetKind {
    GeneralAviation,
    #[default]
    Commercial,
    Fighter,
    Custom,
}

impl PresetKind {
    pub const ALL: [PresetKind; 4] = [
        PresetKind::GeneralAviation,
        PresetKind::Commercial,
        PresetKind::Fighter,
        PresetKind::Custom,
    ];

    pub fn label(self) -> &'static str {
        match self {
            PresetKind::GeneralAviation => "General Aviation",
            PresetKind::Commercial => "Commercial",
            PresetKind::Fighter => "Fighter",
            PresetKind::Custom => "Custom",
        }
    }

    pub fn file_stem(self) -> &'static str {
        match self {
            PresetKind::GeneralAviation => "general_aviation",
            PresetKind::Commercial => "commercial",
            PresetKind::Fighter => "fighter",
            PresetKind::Custom => "custom",
        }
    }

    pub fn is_built_in(self) -> bool {
        !matches!(self, PresetKind::Custom)
    }

    pub fn built_in_default(self) -> Preset {
        let mut rumble = RumbleConfig::default();
        let mut simvars = SimVarProfile::default();

        match self {
            PresetKind::GeneralAviation => {
                rumble.base_airspeed = 12.0;
                rumble.ground_roll = 28.0;
                rumble.flaps_peak = 45.0;
                rumble.gear_peak = 75.0;
                rumble.stall_ceiling = 130.0;
                rumble.bank = 55.0;
                rumble.smoothing_alpha = 0.20;
                rumble.taxi_start_kn = 2.0;
                rumble.taxi_end_kn = 8.0;
                rumble.ias_deadband_kn = 1.0;
                rumble.flaps_bump_duration_s = 1.0;
                rumble.gear_bump_duration_s = 0.8;
            }
            PresetKind::Commercial => {
                rumble.base_airspeed = 18.0;
                rumble.ground_roll = 55.0;
                rumble.flaps_peak = 65.0;
                rumble.gear_peak = 120.0;
                rumble.stall_ceiling = 160.0;
                rumble.bank = 45.0;
                rumble.smoothing_alpha = 0.18;
                rumble.taxi_start_kn = 5.0;
                rumble.taxi_end_kn = 18.0;
                rumble.ias_deadband_kn = 1.0;
                rumble.flaps_bump_duration_s = 1.0;
                rumble.gear_bump_duration_s = 0.8;
                simvars.extra.push(SimVarDef {
                    name: "SPOILERS HANDLE POSITION".to_string(),
                    unit: "Percent".to_string(),
                    key: "spoilers_pct".to_string(),
                });
            }
            PresetKind::Fighter => {
                rumble.base_airspeed = 24.0;
                rumble.ground_roll = 40.0;
                rumble.flaps_peak = 85.0;
                rumble.gear_peak = 100.0;
                rumble.stall_ceiling = 210.0;
                rumble.bank = 115.0;
                rumble.smoothing_alpha = 0.12;
                rumble.taxi_start_kn = 4.0;
                rumble.taxi_end_kn = 12.0;
                rumble.ias_deadband_kn = 0.5;
                rumble.flaps_bump_duration_s = 0.6;
                rumble.gear_bump_duration_s = 0.5;
            }
            PresetKind::Custom => {
                return PresetKind::Commercial.built_in_default();
            }
        }

        Preset {
            kind: self,
            rumble,
            simvars,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimVarDef {
    pub name: String,
    pub unit: String,
    pub key: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimVarProfile {
    #[serde(default)]
    pub extra: Vec<SimVarDef>,
}

impl SimVarProfile {
    pub fn layout(&self) -> SimVarLayout {
        SimVarLayout {
            extra_keys: self.extra.iter().map(|d| d.key.clone()).collect(),
        }
    }

    pub fn all_simvar_defs(&self) -> Vec<(&str, &str)> {
        let mut out: Vec<(&str, &str)> = CORE_SIMVARS.to_vec();
        for def in &self.extra {
            out.push((def.name.as_str(), def.unit.as_str()));
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimVarLayout {
    pub extra_keys: Vec<String>,
}

impl SimVarLayout {
    pub fn core_only() -> Self {
        SimVarProfile::default().layout()
    }

    pub fn total_count(&self) -> usize {
        CORE_SIMVAR_COUNT + self.extra_keys.len()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub kind: PresetKind,
    pub rumble: RumbleConfig,
    #[serde(default)]
    pub simvars: SimVarProfile,
}

impl Preset {
    pub fn layout(&self) -> SimVarLayout {
        self.simvars.layout()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SettingsFile {
    active: PresetKind,
}

pub struct PresetStore {
    dir: PathBuf,
}

impl PresetStore {
    pub fn exe_presets_dir() -> PathBuf {
        if let Ok(exe) = std::env::current_exe() {
            if let Some(parent) = exe.parent() {
                return parent.join("presets");
            }
        }
        PathBuf::from("presets")
    }

    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn at_exe_dir() -> Self {
        Self::new(Self::exe_presets_dir())
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn bootstrap(&self) -> std::io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        for kind in PresetKind::ALL {
            if kind == PresetKind::Custom {
                continue;
            }
            let path = self.preset_path(kind);
            if !path.exists() {
                self.write_preset_file(kind, &kind.built_in_default())?;
            }
        }
        let settings_path = self.settings_path();
        if !settings_path.exists() {
            self.save_active(PresetKind::Commercial)?;
        }
        Ok(())
    }

    fn preset_path(&self, kind: PresetKind) -> PathBuf {
        self.dir.join(format!("{}.yml", kind.file_stem()))
    }

    fn settings_path(&self) -> PathBuf {
        self.dir.join("settings.yml")
    }

    pub fn load(&self, kind: PresetKind) -> Preset {
        let path = self.preset_path(kind);
        if path.exists() {
            if let Ok(text) = fs::read_to_string(&path) {
                if let Ok(mut preset) = serde_yaml::from_str::<Preset>(&text) {
                    preset.kind = kind;
                    return preset;
                }
            }
        }
        kind.built_in_default()
    }

    pub fn save(&self, preset: &Preset) -> std::io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        self.write_preset_file(preset.kind, preset)
    }

    fn write_preset_file(&self, kind: PresetKind, preset: &Preset) -> std::io::Result<()> {
        let path = self.preset_path(kind);
        let mut to_write = preset.clone();
        to_write.kind = kind;
        let text = serde_yaml::to_string(&to_write)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        fs::write(path, text)
    }

    pub fn load_active(&self) -> PresetKind {
        let path = self.settings_path();
        if path.exists() {
            if let Ok(text) = fs::read_to_string(&path) {
                if let Ok(settings) = serde_yaml::from_str::<SettingsFile>(&text) {
                    return settings.active;
                }
            }
        }
        PresetKind::Commercial
    }

    pub fn save_active(&self, kind: PresetKind) -> std::io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let settings = SettingsFile { active: kind };
        let text = serde_yaml::to_string(&settings)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        fs::write(self.settings_path(), text)
    }

    pub fn reset_to_built_in(&self, kind: PresetKind) -> Preset {
        let preset = kind.built_in_default();
        if kind.is_built_in() {
            let _ = self.write_preset_file(kind, &preset);
        }
        preset
    }
}

pub struct PresetShared {
    inner: Mutex<Preset>,
    rev: AtomicU64,
}

impl PresetShared {
    pub fn new(preset: Preset) -> Self {
        Self {
            inner: Mutex::new(preset),
            rev: AtomicU64::new(1),
        }
    }

    pub fn get(&self) -> Preset {
        self.inner.lock().clone()
    }

    pub fn set(&self, v: Preset) {
        *self.inner.lock() = v;
        self.rev.fetch_add(1, Ordering::Relaxed);
    }

    pub fn with_mut_rumble<F: FnOnce(&mut RumbleConfig, PresetKind) -> PresetKind>(&self, f: F) {
        let mut g = self.inner.lock();
        let kind = g.kind;
        g.kind = f(&mut g.rumble, kind);
        self.rev.fetch_add(1, Ordering::Relaxed);
    }

    pub fn rumble_config(&self) -> RumbleConfig {
        self.inner.lock().rumble.clone()
    }

    pub fn layout(&self) -> SimVarLayout {
        self.inner.lock().layout()
    }

    pub fn simvar_profile(&self) -> SimVarProfile {
        self.inner.lock().simvars.clone()
    }

    pub fn kind(&self) -> PresetKind {
        self.inner.lock().kind
    }

    pub fn current_rev(&self) -> u64 {
        self.rev.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_defaults_differ_by_kind() {
        let ga = PresetKind::GeneralAviation.built_in_default();
        let com = PresetKind::Commercial.built_in_default();
        let ftr = PresetKind::Fighter.built_in_default();
        assert_ne!(ga.rumble.base_airspeed, ftr.rumble.base_airspeed);
        assert_eq!(com.simvars.extra.len(), 1);
        assert!(ga.simvars.extra.is_empty());
    }

    #[test]
    fn yaml_roundtrip_built_in_defaults() {
        for kind in [
            PresetKind::GeneralAviation,
            PresetKind::Commercial,
            PresetKind::Fighter,
        ] {
            let preset = kind.built_in_default();
            let yaml = serde_yaml::to_string(&preset).unwrap();
            let parsed: Preset = serde_yaml::from_str(&yaml).unwrap();
            assert_eq!(parsed.kind, kind);
            assert_eq!(parsed.rumble, preset.rumble);
            assert_eq!(parsed.simvars, preset.simvars);
        }
    }
}
