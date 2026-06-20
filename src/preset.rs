use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::hid::protocol::SidestickVariant;
use crate::RumbleConfig;

mod simvars;
pub use simvars::{
    canonical_extras_for, is_engine_extra_key, CORE_SIMVARS,
    CORE_SIMVAR_COUNT,
};

pub const SIMCONNECT_UNUSED_DATUM: u32 = 0xFFFF_FFFF;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PresetKind {
    GeneralAviation,
    #[default]
    Commercial,
    Fighter,
}

impl PresetKind {
    pub const ALL: [PresetKind; 3] = [
        PresetKind::GeneralAviation,
        PresetKind::Commercial,
        PresetKind::Fighter,
    ];

    pub fn label(self) -> &'static str {
        match self {
            PresetKind::GeneralAviation => "General Aviation",
            PresetKind::Commercial => "Commercial",
            PresetKind::Fighter => "Fighter",
        }
    }

    pub fn file_stem(self) -> &'static str {
        match self {
            PresetKind::GeneralAviation => "general_aviation",
            PresetKind::Commercial => "commercial",
            PresetKind::Fighter => "fighter",
        }
    }

    pub fn from_settings_str(s: &str) -> Self {
        match s {
            "general_aviation" => PresetKind::GeneralAviation,
            "commercial" => PresetKind::Commercial,
            "fighter" => PresetKind::Fighter,
            // Legacy: custom preset slot removed; treat as Commercial.
            "custom" => PresetKind::Commercial,
            _ => PresetKind::Commercial,
        }
    }

    pub fn built_in_default(self) -> Preset {
        let mut rumble = RumbleConfig::default();
        let simvars = canonical_extras_for(self);

        match self {
            PresetKind::GeneralAviation => {
                rumble.base_airspeed = 12.0;
                rumble.ground_roll = 28.0;
                rumble.flaps_peak = 45.0;
                rumble.gear_peak = 75.0;
                rumble.stall_ceiling = 130.0;
                rumble.bank = 55.0;
                rumble.spoilers = 25.0;
                rumble.engine_vibe = 10.0;
                rumble.eng_rpm_spool_min = 0.0;
                rumble.eng_rpm_startup_max = 800.0;
                rumble.eng_rpm_idle = 1000.0;
                rumble.eng_rpm_max = 2550.0;
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
                rumble.spoilers = 28.0;
                rumble.engine_vibe = 16.0;
                rumble.engine_idle_n1_pct = 22.0;
                rumble.eng_rpm_spool_min = 800.0;
                rumble.eng_rpm_startup_max = 900.0;
                rumble.eng_rpm_idle = 2500.0;
                rumble.eng_rpm_max = 5200.0;
                rumble.smoothing_alpha = 0.18;
                rumble.taxi_start_kn = 5.0;
                rumble.taxi_end_kn = 18.0;
                rumble.ias_deadband_kn = 1.0;
                rumble.flaps_bump_duration_s = 1.0;
                rumble.gear_bump_duration_s = 0.45;
            }
            PresetKind::Fighter => {
                rumble.base_airspeed = 24.0;
                rumble.ground_roll = 40.0;
                rumble.flaps_peak = 85.0;
                rumble.gear_peak = 100.0;
                rumble.stall_ceiling = 210.0;
                rumble.bank = 115.0;
                rumble.spoilers = 35.0;
                rumble.engine_vibe = 14.0;
                rumble.engine_idle_n1_pct = 58.0;
                rumble.eng_rpm_spool_min = 600.0;
                rumble.eng_rpm_startup_max = 900.0;
                rumble.eng_rpm_idle = 2800.0;
                rumble.eng_rpm_max = 7500.0;
                rumble.smoothing_alpha = 0.12;
                rumble.taxi_start_kn = 4.0;
                rumble.taxi_end_kn = 12.0;
                rumble.ias_deadband_kn = 0.5;
                rumble.flaps_bump_duration_s = 0.6;
                rumble.gear_bump_duration_s = 0.5;
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
    #[serde(default = "default_datum_index")]
    pub datum_index: u32,
}

fn default_datum_index() -> u32 {
    SIMCONNECT_UNUSED_DATUM
}

impl SimVarDef {
    pub fn normalize_datum_suffix(&mut self) {
        if self.datum_index != SIMCONNECT_UNUSED_DATUM {
            return;
        }
        if let Some(pos) = self.name.rfind(':') {
            if let Ok(idx) = self.name[pos + 1..].parse::<u32>() {
                self.datum_index = idx;
                self.name = self.name[..pos].to_string();
            }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimVarProfile {
    #[serde(default)]
    pub extra: Vec<SimVarDef>,
}

impl SimVarProfile {
    pub fn normalize(&mut self) {
        for def in &mut self.extra {
            def.normalize_datum_suffix();
        }
    }

    pub fn layout(&self) -> SimVarLayout {
        SimVarLayout::core_only()
            .with_extra_keys(self.extra.iter().map(|d| d.key.clone()).collect::<Vec<_>>())
    }

    pub fn all_simvar_entries(&self) -> Vec<(&str, &str, u32)> {
        let mut out: Vec<(&str, &str, u32)> = CORE_SIMVARS
            .iter()
            .map(|(n, u)| (*n, *u, SIMCONNECT_UNUSED_DATUM))
            .collect();
        for def in &self.extra {
            out.push((def.name.as_str(), def.unit.as_str(), def.datum_index));
        }
        out
    }

    /// SimConnect datum name: indexed simvars use `:N` suffix (MSFS SDK); datum ID stays UNUSED.
    pub fn simconnect_datum_name(name: &str, datum_index: u32) -> String {
        if datum_index == SIMCONNECT_UNUSED_DATUM {
            name.to_string()
        } else {
            format!("{name}:{datum_index}")
        }
    }

    pub fn all_simvar_defs(&self) -> Vec<(&str, &str)> {
        self.all_simvar_entries()
            .into_iter()
            .map(|(n, u, _)| (n, u))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutField {
    AirspeedIndicated,
    OnGround,
    BankDegrees,
    FlapsLeftPct,
    FlapsRightPct,
    FlapsIndex,
    GearHandle,
    StallWarning,
    SimTime,
    GroundSpeed,
    Paused,
    Extra(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimVarLayout {
    pub fields: Vec<LayoutField>,
}

fn core_layout_fields() -> [LayoutField; CORE_SIMVAR_COUNT] {
    [
        LayoutField::AirspeedIndicated,
        LayoutField::OnGround,
        LayoutField::BankDegrees,
        LayoutField::FlapsLeftPct,
        LayoutField::FlapsRightPct,
        LayoutField::FlapsIndex,
        LayoutField::SimTime,
        LayoutField::Paused,
    ]
}

impl SimVarLayout {
    pub fn core_only() -> Self {
        Self {
            fields: core_layout_fields().to_vec(),
        }
    }

    pub fn with_extra_keys(mut self, keys: Vec<String>) -> Self {
        for key in keys {
            self.fields.push(LayoutField::Extra(key));
        }
        self
    }

    pub fn total_count(&self) -> usize {
        self.fields.len()
    }

    pub fn extra_keys(&self) -> Vec<String> {
        self.fields
            .iter()
            .filter_map(|f| match f {
                LayoutField::Extra(k) => Some(k.clone()),
                _ => None,
            })
            .collect()
    }
}

/// On-disk preset: slider values only. SimConnect simvars stay in code defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct PresetFile {
    pub kind: PresetKind,
    pub rumble: RumbleConfig,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Preset {
    pub kind: PresetKind,
    pub rumble: RumbleConfig,
    pub simvars: SimVarProfile,
}

impl Preset {
    fn to_file(&self) -> PresetFile {
        PresetFile {
            kind: self.kind,
            rumble: self.rumble.clone(),
        }
    }

    pub fn layout(&self) -> SimVarLayout {
        self.simvars.layout()
    }

    pub fn apply_canonical_simvars(&mut self, kind: PresetKind) {
        let canonical = kind.built_in_default();
        self.simvars.apply_canonical_extras(&canonical.simvars);
    }

    pub fn merge_rumble_from(&mut self, default: &Preset) {
        if self.rumble.eng_rpm_spool_min <= 0.0 {
            self.rumble.eng_rpm_spool_min = default.rumble.eng_rpm_spool_min;
        }
        if self.rumble.eng_rpm_startup_max <= 0.0 {
            self.rumble.eng_rpm_startup_max = default.rumble.eng_rpm_startup_max;
        }
        if self.rumble.eng_rpm_idle <= 0.0 {
            self.rumble.eng_rpm_idle = default.rumble.eng_rpm_idle;
        }
        if self.rumble.eng_rpm_max <= self.rumble.eng_rpm_idle {
            self.rumble.eng_rpm_max = default.rumble.eng_rpm_max;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppSettings {
    pub active: PresetKind,
    #[serde(default = "default_show_live_aircraft_data")]
    pub show_live_aircraft_data: bool,
    #[serde(default)]
    pub sidestick_variant: SidestickVariant,
}

fn default_show_live_aircraft_data() -> bool {
    true
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            active: PresetKind::Commercial,
            show_live_aircraft_data: true,
            sidestick_variant: SidestickVariant::Airbus,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct SettingsFile {
    active: String,
    #[serde(default = "default_show_live_aircraft_data")]
    show_live_aircraft_data: bool,
    #[serde(default)]
    sidestick_variant: String,
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
        let settings_path = self.settings_path();
        if !settings_path.exists() {
            self.save_settings(&AppSettings::default())?;
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
        let default = kind.built_in_default();
        let path = self.preset_path(kind);

        if !path.exists() {
            return default;
        }

        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(_) => return default,
        };

        let from_disk = serde_yaml::from_str::<PresetFile>(&text).ok();

        let Some(from_disk) = from_disk else {
            return default;
        };

        let mut preset = default;
        preset.rumble = from_disk.rumble;
        preset.merge_rumble_from(&kind.built_in_default());
        preset.kind = kind;

        if text.contains("simvars:") {
            let _ = self.write_preset_file(kind, &preset);
        }

        preset
    }

    pub fn save(&self, preset: &Preset) -> std::io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        self.write_preset_file(preset.kind, preset)
    }

    fn write_preset_file(&self, kind: PresetKind, preset: &Preset) -> std::io::Result<()> {
        let path = self.preset_path(kind);
        let mut file = preset.to_file();
        file.kind = kind;
        let text = serde_yaml::to_string(&file)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        fs::write(path, text)
    }

    pub fn load_settings(&self) -> AppSettings {
        let path = self.settings_path();
        if path.exists() {
            if let Ok(text) = fs::read_to_string(&path) {
                if let Ok(settings) = serde_yaml::from_str::<SettingsFile>(&text) {
                    return AppSettings {
                        active: PresetKind::from_settings_str(&settings.active),
                        show_live_aircraft_data: settings.show_live_aircraft_data,
                        sidestick_variant: SidestickVariant::from_settings_str(
                            &settings.sidestick_variant,
                        ),
                    };
                }
            }
        }
        AppSettings::default()
    }

    pub fn save_settings(&self, settings: &AppSettings) -> std::io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let text = serde_yaml::to_string(settings)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        fs::write(self.settings_path(), text)
    }

    pub fn load_active(&self) -> PresetKind {
        self.load_settings().active
    }

    pub fn save_active(&self, kind: PresetKind) -> std::io::Result<()> {
        let mut settings = self.load_settings();
        settings.active = kind;
        self.save_settings(&settings)
    }

    pub fn reset_to_built_in(&self, kind: PresetKind) -> Preset {
        let path = self.preset_path(kind);
        let _ = fs::remove_file(path);
        kind.built_in_default()
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
        assert_eq!(com.simvars.extra.len(), 20);
        assert_eq!(ga.simvars.extra.len(), 17);
        assert_eq!(ftr.simvars.extra.len(), 15);
    }

    #[test]
    fn normalize_strips_colon_index_from_legacy_simvar_names() {
        let mut def = SimVarDef {
            name: "TURB ENG N1:1".to_string(),
            unit: "Percent".to_string(),
            key: "eng_n1_1".to_string(),
            datum_index: SIMCONNECT_UNUSED_DATUM,
        };
        def.normalize_datum_suffix();
        assert_eq!(def.name, "TURB ENG N1");
        assert_eq!(def.datum_index, 1);
    }

    #[test]
    fn load_ignores_legacy_disk_simvars() {
        let dir = std::env::temp_dir().join(format!("ursa-merge-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let store = PresetStore::new(dir.clone());
        store.bootstrap().unwrap();

        let mut saved = PresetKind::Commercial.built_in_default();
        saved.rumble.base_airspeed = 42.0;
        saved.simvars.extra.retain(|d| d.key == "spoilers_pct");
        store.save(&saved).unwrap();

        let text = fs::read_to_string(dir.join("commercial.yml")).unwrap();
        assert!(!text.contains("simvars:"));

        let loaded = store.load(PresetKind::Commercial);
        assert_eq!(loaded.rumble.base_airspeed, 42.0);
        assert_eq!(loaded.simvars.extra.len(), 20);
        assert!(loaded.simvars.extra.iter().any(|d| d.key == "eng_rpm_1"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_reads_rumble_from_legacy_yaml_with_simvars() {
        let dir = std::env::temp_dir().join(format!("ursa-legacy-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let mut legacy = PresetKind::Commercial.built_in_default();
        legacy.rumble.base_airspeed = 77.0;
        let mut yaml = serde_yaml::to_string(&legacy.to_file()).unwrap();
        yaml.push_str("simvars:\n  extra:\n  - name: SPOILERS\n    unit: Percent\n    key: spoilers_pct\n");

        fs::write(dir.join("commercial.yml"), yaml).unwrap();

        let store = PresetStore::new(dir.clone());
        let loaded = store.load(PresetKind::Commercial);
        assert_eq!(loaded.rumble.base_airspeed, 77.0);
        assert_eq!(loaded.simvars, PresetKind::Commercial.built_in_default().simvars);

        let rewritten = fs::read_to_string(dir.join("commercial.yml")).unwrap();
        assert!(!rewritten.contains("simvars:"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn yaml_roundtrip_built_in_defaults() {
        for kind in [
            PresetKind::GeneralAviation,
            PresetKind::Commercial,
            PresetKind::Fighter,
        ] {
            let preset = kind.built_in_default();
            let yaml = serde_yaml::to_string(&preset.to_file()).unwrap();
            assert!(!yaml.contains("simvars:"));
            let parsed: PresetFile = serde_yaml::from_str(&yaml).unwrap();
            assert_eq!(parsed.kind, kind);
            assert_eq!(parsed.rumble, preset.rumble);
        }
    }

    #[test]
    fn indexed_extras_use_colon_suffix_for_simconnect() {
        let ga = PresetKind::GeneralAviation.built_in_default();
        let rpm = ga
            .simvars
            .extra
            .iter()
            .find(|d| d.key == "eng_rpm_1")
            .expect("eng_rpm_1");
        assert_eq!(rpm.name, "GENERAL ENG RPM");
        assert_eq!(rpm.datum_index, 1);
        assert_eq!(
            SimVarProfile::simconnect_datum_name(&rpm.name, rpm.datum_index),
            "GENERAL ENG RPM:1"
        );
        assert_eq!(
            SimVarProfile::simconnect_datum_name("PAUSED", SIMCONNECT_UNUSED_DATUM),
            "PAUSED"
        );
    }

    #[test]
    fn save_writes_rumble_only() {
        let dir = std::env::temp_dir().join(format!("ursa-order-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let store = PresetStore::new(dir.clone());
        store.bootstrap().unwrap();

        let mut preset = PresetKind::GeneralAviation.built_in_default();
        preset.rumble.base_airspeed = 33.0;
        preset.simvars.extra.swap(0, 1);
        store.save(&preset).unwrap();

        let text = fs::read_to_string(dir.join("general_aviation.yml")).unwrap();
        assert!(!text.contains("simvars:"));
        assert!(text.contains("base_airspeed: 33"));

        let loaded = store.load(PresetKind::GeneralAviation);
        assert_eq!(loaded.rumble.base_airspeed, 33.0);
        assert_eq!(
            loaded.simvars.extra,
            PresetKind::GeneralAviation.built_in_default().simvars.extra
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reset_deletes_disk_override() {
        let dir = std::env::temp_dir().join(format!("ursa-reset-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let store = PresetStore::new(dir.clone());
        store.bootstrap().unwrap();

        let mut custom = PresetKind::Commercial.built_in_default();
        custom.rumble.base_airspeed = 99.0;
        store.save(&custom).unwrap();

        let path = dir.join("commercial.yml");
        assert!(path.exists());

        let reset = store.reset_to_built_in(PresetKind::Commercial);
        assert!(!path.exists());
        assert_eq!(reset.rumble.base_airspeed, 18.0);

        let loaded = store.load(PresetKind::Commercial);
        assert_eq!(loaded.rumble.base_airspeed, 18.0);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_active_migrates_legacy_custom() {
        let dir = std::env::temp_dir().join(format!("ursa-active-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let store = PresetStore::new(dir.clone());
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("settings.yml"), "active: custom\n").unwrap();

        let settings = store.load_settings();
        assert_eq!(settings.active, PresetKind::Commercial);
        assert!(settings.show_live_aircraft_data);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn settings_roundtrip_preserves_show_live_aircraft_data() {
        let dir = std::env::temp_dir().join(format!("ursa-settings-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let store = PresetStore::new(dir.clone());
        store.bootstrap().unwrap();

        let mut settings = store.load_settings();
        settings.show_live_aircraft_data = false;
        store.save_settings(&settings).unwrap();

        let loaded = store.load_settings();
        assert_eq!(loaded.active, PresetKind::Commercial);
        assert!(!loaded.show_live_aircraft_data);

        store.save_active(PresetKind::Fighter).unwrap();
        let after_preset_change = store.load_settings();
        assert_eq!(after_preset_change.active, PresetKind::Fighter);
        assert!(!after_preset_change.show_live_aircraft_data);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn settings_roundtrip_preserves_sidestick_variant() {
        let dir =
            std::env::temp_dir().join(format!("ursa-settings-variant-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let store = PresetStore::new(dir.clone());
        store.bootstrap().unwrap();

        let mut settings = store.load_settings();
        settings.sidestick_variant = SidestickVariant::Fighter;
        store.save_settings(&settings).unwrap();

        let loaded = store.load_settings();
        assert_eq!(loaded.sidestick_variant, SidestickVariant::Fighter);

        store.save_active(PresetKind::GeneralAviation).unwrap();
        let after_preset_change = store.load_settings();
        assert_eq!(after_preset_change.active, PresetKind::GeneralAviation);
        assert_eq!(
            after_preset_change.sidestick_variant,
            SidestickVariant::Fighter
        );

        let _ = fs::remove_dir_all(&dir);
    }
}
