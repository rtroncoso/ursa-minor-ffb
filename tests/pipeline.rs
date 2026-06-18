use ursa_minor_ffb::hid::protocol::{build_simapp_vibe_frame, WW_PID_URSA_MINOR_AIRBUS_L};
use ursa_minor_ffb::rumble::RumbleEngine;
use ursa_minor_ffb::sim::parse::{flight_status, parse_main_elems};
use ursa_minor_ffb::{FlightVars, RumbleConfig, SimStatus};

mod support;

fn elems_from_flight(
    ias: f64,
    on_ground: f64,
    bank: f64,
    flaps_l: f64,
    flaps_r: f64,
    flaps_idx: f64,
    gear: f64,
    stalled: f64,
    sim_time: f64,
    gs: f64,
    paused: f64,
) -> [f64; 11] {
    [
        ias, on_ground, bank, flaps_l, flaps_r, flaps_idx, gear, stalled, sim_time, gs, paused,
    ]
}

#[test]
fn pipeline_ground_taxi_to_takeoff() {
    let cfg = RumbleConfig::default();
    let mut engine = RumbleEngine::new();

    let taxi_elems = elems_from_flight(0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.05, 5.0, 0.0);
    let taxi_fv = parse_main_elems(&taxi_elems, false, cfg.ias_deadband_kn);
    assert_eq!(flight_status(&taxi_fv), SimStatus::Connected);

    let taxi_out = engine.step(&taxi_fv, &cfg, 1, false);
    assert!(taxi_out.effects.ground_thump_active);
    assert!(taxi_out.intensity > 0);

    let taxi_frame =
        build_simapp_vibe_frame(WW_PID_URSA_MINOR_AIRBUS_L, 0x02, 14, taxi_out.intensity);
    assert_eq!(taxi_frame[0], 0x02);
    assert_eq!(taxi_frame[8], taxi_out.intensity);

    let takeoff_elems = elems_from_flight(120.0, 0.0, 5.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0);
    let takeoff_fv = parse_main_elems(&takeoff_elems, false, cfg.ias_deadband_kn);
    assert_eq!(flight_status(&takeoff_fv), SimStatus::InFlight);

    let takeoff_out = engine.step(&takeoff_fv, &cfg, 1, false);
    assert!(takeoff_out.effects.base_active);
    assert!(takeoff_out.intensity > 0);
}

#[test]
fn pipeline_flap_change_during_flight() {
    let cfg = RumbleConfig::default();
    let mut engine = RumbleEngine::new();

    let cruise = elems_from_flight(150.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 10.0, 0.0, 0.0);
    let cruise_fv = parse_main_elems(&cruise, false, cfg.ias_deadband_kn);
    let _ = engine.step(&cruise_fv, &cfg, 1, false);

    let flaps = elems_from_flight(150.0, 0.0, 0.0, 50.0, 50.0, 2.0, 0.0, 0.0, 10.1, 0.0, 0.0);
    let flaps_fv = parse_main_elems(&flaps, false, cfg.ias_deadband_kn);
    let out = engine.step(&flaps_fv, &cfg, 1, false);

    assert!(out.effects.flaps_bump_active);
    let frame = build_simapp_vibe_frame(WW_PID_URSA_MINOR_AIRBUS_L, 0x02, 14, out.intensity);
    assert_eq!(frame[2], 0xBF);
    assert_eq!(frame[8], out.intensity);
}

#[test]
fn pipeline_stall_ceiling() {
    let cfg = RumbleConfig::default();
    let mut engine = RumbleEngine::new();

    let stall_elems = elems_from_flight(80.0, 0.0, 30.0, 0.0, 0.0, 0.0, 0.0, 1.0, 2.0, 0.0, 0.0);
    let stall_fv = parse_main_elems(&stall_elems, false, cfg.ias_deadband_kn);
    let out = engine.step(&stall_fv, &cfg, 1, false);

    assert!(out.effects.stall_active);
    assert!(out.intensity >= cfg.stall_ceiling as u8);
}

#[test]
fn pipeline_pause_zeros_output() {
    let cfg = RumbleConfig::default();
    let mut engine = RumbleEngine::new();

    let flying = elems_from_flight(200.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0);
    let fv = parse_main_elems(&flying, false, cfg.ias_deadband_kn);
    let active = engine.step(&fv, &cfg, 1, false);
    assert!(active.intensity > 0);

    let paused_fv = parse_main_elems(&flying, true, cfg.ias_deadband_kn);
    let paused = engine.step(&paused_fv, &cfg, 1, false);
    assert_eq!(paused.intensity, 0);

    let held = engine.step(&fv, &cfg, 1, true);
    assert_eq!(held.intensity, 0);
}

#[test]
fn pipeline_gear_retraction_bump() {
    let cfg = RumbleConfig::default();
    let mut engine = RumbleEngine::new();

    let gear_down = elems_from_flight(150.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 5.0, 0.0, 0.0);
    let down_fv = parse_main_elems(&gear_down, false, cfg.ias_deadband_kn);
    let _ = engine.step(&down_fv, &cfg, 1, false);

    let gear_up = elems_from_flight(150.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 5.05, 0.0, 0.0);
    let up_fv = parse_main_elems(&gear_up, false, cfg.ias_deadband_kn);
    let out = engine.step(&up_fv, &cfg, 1, false);

    assert!(out.effects.gear_bump_active);
}

#[test]
fn pipeline_scripted_timeline_via_support() {
    let timeline = support::scripted_flight_timeline();
    let cfg = RumbleConfig::default();
    let mut engine = RumbleEngine::new();
    let mut intensities = Vec::new();

    for (elems, paused_events) in timeline {
        let fv = parse_main_elems(&elems, paused_events, cfg.ias_deadband_kn);
        let out = engine.step(&fv, &cfg, 1, false);
        intensities.push(out.intensity);
    }

    assert!(intensities.iter().any(|&i| i > 0));
    assert_eq!(intensities.last(), Some(&0));
}

#[test]
fn pipeline_frame_encoding_matches_intensity_at_each_step() {
    let cfg = RumbleConfig::default();
    let mut engine = RumbleEngine::new();
    let steps = [
        elems_from_flight(0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.1, 6.0, 0.0),
        elems_from_flight(100.0, 0.0, 10.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0),
    ];

    for elems in steps {
        let fv = parse_main_elems(&elems, false, cfg.ias_deadband_kn);
        let out = engine.step(&fv, &cfg, 1, false);
        let frame = build_simapp_vibe_frame(WW_PID_URSA_MINOR_AIRBUS_L, 0x02, 14, out.intensity);
        assert_eq!(frame[8], out.intensity);
    }
}

#[test]
fn config_shared_rev_triggers_smoothing_reset() {
    use std::sync::Arc;
    use ursa_minor_ffb::ConfigShared;

    let shared = Arc::new(ConfigShared::new());
    let cfg1 = shared.get();
    let rev1 = shared.current_rev();

    shared.with_mut(|c| c.base_airspeed = 32.0);
    let rev2 = shared.current_rev();
    assert!(rev2 > rev1);

    let mut engine = RumbleEngine::new();
    let fv = FlightVars {
        airspeed_indicated: 200.0,
        on_ground: false,
        sim_time_s: 1.0,
        ..Default::default()
    };

    let _ = engine.step(&fv, &cfg1, rev1, false);
    let out = engine.step(&fv, &shared.get(), rev2, false);
    assert!(out.intensity > 0);
}
