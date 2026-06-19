use std::fs;

use ursa_minor_ffb::preset::{PresetKind, PresetStore};
use ursa_minor_ffb::sim::parse::parse_main_elems;
use ursa_minor_ffb::SimVarLayout;

#[test]
fn preset_store_bootstrap_creates_built_in_files() {
    let dir = std::env::temp_dir().join(format!("ursa-presets-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let store = PresetStore::new(dir.clone());
    store.bootstrap().unwrap();

    for kind in [
        PresetKind::GeneralAviation,
        PresetKind::Commercial,
        PresetKind::Fighter,
    ] {
        let path = dir.join(format!("{}.yml", kind.file_stem()));
        assert!(path.exists(), "missing {}", path.display());
        let preset = store.load(kind);
        assert_eq!(preset.kind, kind);
    }

    assert!(dir.join("settings.yml").exists());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn commercial_yaml_roundtrip_matches_built_in_default() {
    let preset = PresetKind::Commercial.built_in_default();
    let yaml = serde_yaml::to_string(&preset).unwrap();
    let parsed: ursa_minor_ffb::Preset = serde_yaml::from_str(&yaml).unwrap();
    assert_eq!(parsed.rumble, preset.rumble);
    assert_eq!(parsed.simvars.extra.len(), 13);
    assert_eq!(parsed.simvars.extra[0].key, "spoilers_pct");
    assert_eq!(parsed.simvars.extra[1].key, "wind_kt");
}

#[test]
fn commercial_layout_parses_spoilers_extra() {
    let preset = PresetKind::Commercial.built_in_default();
    let layout = preset.layout();
    let mut elems = vec![0.0; layout.total_count()];
    elems[0] = 120.0;
    elems[12] = 80.0;

    let fv = parse_main_elems(&elems, &layout, false, preset.rumble.ias_deadband_kn);
    assert_eq!(fv.extras.get("spoilers_pct"), Some(&80.0));
}

#[test]
fn preset_store_save_and_load_custom() {
    let dir = std::env::temp_dir().join(format!("ursa-presets-custom-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let store = PresetStore::new(dir.clone());
    store.bootstrap().unwrap();

    let mut custom = PresetKind::Commercial.built_in_default();
    custom.kind = PresetKind::Custom;
    custom.rumble.base_airspeed = 99.0;
    store.save(&custom).unwrap();

    let loaded = store.load(PresetKind::Custom);
    assert_eq!(loaded.kind, PresetKind::Custom);
    assert_eq!(loaded.rumble.base_airspeed, 99.0);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn core_only_layout_has_twelve_fields() {
    let layout = SimVarLayout::core_only();
    assert_eq!(layout.total_count(), 12);
}
