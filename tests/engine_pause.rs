//! Regression: MSFS reports PAUSED=true while parked with a running engine.
//!
//! Bug: pause was evaluated before `eng_rpm` merged from extras, then rumble early-returned
//! zero intensity — engine dot stayed gray until the aircraft moved (GS > 3 kt override).
//!
//! Fix: `finalize_flight_vars()` after extras merge + engine-running pause override.

use std::collections::HashMap;

use ursa_minor_ffb::rumble::RumbleEngine;
use ursa_minor_ffb::sim::parse::{finalize_flight_vars, merge_extras, parse_main_elems};
use ursa_minor_ffb::{PresetKind, RumbleConfig, SimVarLayout};

fn ga_rumble() -> RumbleConfig {
    PresetKind::GeneralAviation.built_in_default().rumble
}

fn parked_core_elems(paused_simvar: f64) -> [f64; 13] {
    [
        0.0,  // IAS
        1.0,  // on ground
        0.0,  // bank
        0.0,
        0.0,  // flaps
        0.0,  // flaps idx
        1.0,  // gear down
        0.0,  // stall
        10.0, // sim time
        0.0,  // GS
        paused_simvar,
        0.0,  // wind kt
        0.0,  // wind dir
    ]
}

#[test]
fn parked_running_engine_survives_msfs_false_pause() {
    let cfg = ga_rumble();
    let layout = SimVarLayout::core_only();

    let mut fv = parse_main_elems(&parked_core_elems(1.0), &layout, false, cfg.ias_deadband_kn);
    assert!(fv.paused, "raw PAUSED simvar");

    let mut extras = HashMap::new();
    extras.insert("eng_rpm_1".to_string(), 1405.0);
    merge_extras(&mut fv, &extras);
    finalize_flight_vars(&mut fv);

    assert!(
        !fv.paused,
        "parked engine must clear false pause before rumble step"
    );
    assert_eq!(fv.eng_rpm, 1405.0);

    let mut engine = RumbleEngine::new();
    let out = engine.step(&fv, &cfg, 1, false);

    assert!(
        out.effects.engine_vibe_active,
        "engine dot must be active when parked with RPM"
    );
    assert!(
        out.intensity > 0,
        "HID intensity must be non-zero (got {})",
        out.intensity
    );
    assert!(
        !out.effects.ground_thump_active && !out.effects.ground_active,
        "only engine effect should be active while parked"
    );
}

#[test]
fn parked_no_engine_stays_muted_when_paused() {
    let cfg = ga_rumble();
    let layout = SimVarLayout::core_only();

    let mut fv = parse_main_elems(&parked_core_elems(1.0), &layout, false, cfg.ias_deadband_kn);
    finalize_flight_vars(&mut fv);

    assert!(fv.paused);
    assert_eq!(fv.eng_rpm, 0.0);

    let mut engine = RumbleEngine::new();
    let out = engine.step(&fv, &cfg, 1, false);
    assert_eq!(out.intensity, 0);
    assert!(!out.effects.engine_vibe_active);
}

#[test]
fn finalize_order_matters_eng_rpm_must_merge_first() {
    let cfg = ga_rumble();
    let layout = SimVarLayout::core_only();

    let mut fv = parse_main_elems(&parked_core_elems(1.0), &layout, false, cfg.ias_deadband_kn);
    finalize_flight_vars(&mut fv);
    assert!(
        fv.paused,
        "core-only frame has no eng_rpm yet — must stay paused"
    );

    let mut extras = HashMap::new();
    extras.insert("eng_rpm_1".to_string(), 1405.0);
    merge_extras(&mut fv, &extras);
    finalize_flight_vars(&mut fv);
    assert!(!fv.paused);
}
