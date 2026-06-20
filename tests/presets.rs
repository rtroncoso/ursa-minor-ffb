use std::fs;

use ursa_minor_ffb::preset::{PresetKind, PresetStore};
use ursa_minor_ffb::sim::parse::parse_main_elems;
use ursa_minor_ffb::SimVarLayout;

#[test]
fn preset_store_bootstrap_creates_settings_only() {
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
        assert!(
            !path.exists(),
            "bootstrap should not create {}",
            path.display()
        );
        let preset = store.load(kind);
        assert_eq!(preset.kind, kind);
        assert_eq!(preset, kind.built_in_default());
    }

    assert!(dir.join("settings.yml").exists());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn commercial_yaml_save_contains_rumble_only() {
    let preset = PresetKind::Commercial.built_in_default();
    let dir = std::env::temp_dir().join(format!("ursa-yaml-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let store = PresetStore::new(dir.clone());
    store.bootstrap().unwrap();
    store.save(&preset).unwrap();

    let loaded = store.load(PresetKind::Commercial);
    let yaml = fs::read_to_string(dir.join("commercial.yml")).unwrap();
    assert!(!yaml.contains("simvars:"));
    assert_eq!(loaded.rumble, preset.rumble);
    assert_eq!(preset.simvars.extra.len(), 20);
    assert_eq!(preset.simvars.extra[0].key, "num_engines");
    assert_eq!(preset.simvars.extra[11].key, "spoilers_pct");
    assert_eq!(preset.simvars.extra[12].key, "vertical_speed_fpm");
    assert_eq!(preset.simvars.extra[13].key, "ground_speed_kt");
    assert_eq!(preset.simvars.extra[15].key, "stall_warning");
    assert_eq!(preset.simvars.extra[18].key, "wind_kt");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn commercial_layout_parses_spoilers_extra() {
    let preset = PresetKind::Commercial.built_in_default();
    let layout = preset.layout();
    let mut elems = vec![0.0; layout.total_count()];
    elems[0] = 120.0;
    elems[19] = 80.0;

    let fv = parse_main_elems(&elems, &layout, false, preset.rumble.ias_deadband_kn);
    assert_eq!(fv.extras.get("spoilers_pct"), Some(&80.0));
}

#[test]
fn preset_store_save_and_load_overrides_code_defaults() {
    let dir = std::env::temp_dir().join(format!("ursa-presets-save-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let store = PresetStore::new(dir.clone());
    store.bootstrap().unwrap();

    let mut saved = PresetKind::Commercial.built_in_default();
    saved.rumble.base_airspeed = 99.0;
    store.save(&saved).unwrap();

    let text = fs::read_to_string(dir.join("commercial.yml")).unwrap();
    assert!(!text.contains("simvars:"));

    let loaded = store.load(PresetKind::Commercial);
    assert_eq!(loaded.kind, PresetKind::Commercial);
    assert_eq!(loaded.rumble.base_airspeed, 99.0);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn core_only_layout_has_eight_fields() {
    let layout = SimVarLayout::core_only();
    assert_eq!(layout.total_count(), 8);
}
