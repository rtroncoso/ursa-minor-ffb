use std::collections::HashMap;

use crate::preset::{LayoutField, SimVarLayout};
use crate::{FlightVars, RumbleConfig, SimStatus};

/// MSFS sets `PAUSED=true` while stationary even during active flight.
///
/// Regression guard: pause must be finalized **after** extras merge so `eng_rpm` is
/// available. Otherwise parked engine rumble is silenced. See `finalize_flight_vars` and
/// `tests/engine_pause.rs`.
pub fn parse_main_elems(
    elem: &[f64],
    layout: &SimVarLayout,
    _paused_from_events: bool,
    ias_deadband_kn: f64,
) -> FlightVars {
    let mut fv = FlightVars {
        airspeed_indicated: 0.0,
        on_ground: false,
        bank_deg: 0.0,
        flaps_pct: 0.0,
        flaps_index: 0,
        gear_handle: 0.0,
        stalled: false,
        sim_time_s: 0.0,
        ground_speed_kt: 0.0,
        wind_kt: 0.0,
        wind_dir_deg: 0.0,
        vertical_speed_fpm: 0.0,
        paused: false,
        eng_rpm: 0.0,
        num_engines: 0,
        extras: HashMap::new(),
    };

    let mut paused_from_var = false;

    for (i, field) in layout.fields.iter().enumerate() {
        let v = elem.get(i).copied().unwrap_or(0.0);
        match field {
            LayoutField::AirspeedIndicated => fv.airspeed_indicated = v,
            LayoutField::OnGround => fv.on_ground = v != 0.0,
            LayoutField::BankDegrees => fv.bank_deg = v,
            LayoutField::FlapsLeftPct | LayoutField::FlapsRightPct => {}
            LayoutField::FlapsIndex => fv.flaps_index = v.round() as i32,
            LayoutField::GearHandle => fv.gear_handle = v,
            LayoutField::StallWarning => fv.stalled = v != 0.0,
            LayoutField::SimTime => fv.sim_time_s = v,
            LayoutField::GroundSpeed => fv.ground_speed_kt = v.max(0.0),
            LayoutField::Paused => paused_from_var = v != 0.0,
            LayoutField::Extra(key) => {
                if v.is_finite() {
                    fv.extras.insert(key.clone(), v);
                }
            }
        }
    }

    let flaps_l = elem
        .iter()
        .enumerate()
        .find_map(|(i, &v)| match layout.fields.get(i) {
            Some(LayoutField::FlapsLeftPct) => Some(v),
            _ => None,
        })
        .unwrap_or(0.0);
    let flaps_r = elem
        .iter()
        .enumerate()
        .find_map(|(i, &v)| match layout.fields.get(i) {
            Some(LayoutField::FlapsRightPct) => Some(v),
            _ => None,
        })
        .unwrap_or(0.0);
    fv.flaps_pct = ((flaps_l + flaps_r) * 0.5).clamp(0.0, 100.0);

    fv.paused = paused_from_var;

    sanitize_flight_vars(&mut fv, ias_deadband_kn);
    sync_aircraft_meta(&mut fv);
    fv
}

/// Recompute pause after core + extras are merged (eng_rpm / wind must be current).
pub fn finalize_flight_vars(fv: &mut FlightVars) {
    sync_aircraft_meta(fv);
    sanitize_wind_fields(fv);
    fv.paused = effective_paused(fv.paused, fv);
}

/// Sync engine RPM (max of indexed engines), wind, and engine count from extras.
pub fn sync_aircraft_meta(fv: &mut FlightVars) {
    sync_eng_rpm(fv);
    sync_wind_from_extras(fv);
    sync_motion_fields(fv);
    if let Some(n) = fv.extras.get("num_engines").copied() {
        if n.is_finite() && n >= 1.0 && n <= 4.0 {
            fv.num_engines = n.round().max(0.0) as u32;
        } else {
            fv.num_engines = 0;
        }
    }
}

fn sync_motion_fields(fv: &mut FlightVars) {
    if let Some(&vs) = fv.extras.get("vertical_speed_fpm") {
        if vs.is_finite() {
            fv.vertical_speed_fpm = vs;
        }
    }

    if let Some(gs) = fv
        .extras
        .get("ground_speed_kt")
        .copied()
        .or_else(|| fv.extras.get("surface_ground_speed_kt").copied())
    {
        if gs.is_finite() && gs >= 0.0 {
            fv.ground_speed_kt = gs;
        }
    }

    if let Some(&raw) = fv.extras.get("stall_warning") {
        fv.stalled = raw != 0.0 && fv.airspeed_indicated >= 40.0;
    } else {
        fv.stalled = false;
    }

    if let Some(&v) = fv.extras.get("gear_handle_bool") {
        if v.is_finite() {
            fv.gear_handle = if v > 1.5 { v / 100.0 } else { v };
        }
    } else if let Some(&v) = fv.extras.get("gear_handle_index") {
        if v.is_finite() {
            fv.gear_handle = v;
        }
    }
}

fn sync_wind_from_extras(fv: &mut FlightVars) {
    if let Some(&kt) = fv.extras.get("wind_kt") {
        if kt.is_finite() && kt >= 0.0 {
            fv.wind_kt = kt;
        }
    }
    if let Some(&dir) = fv.extras.get("wind_dir_deg") {
        if dir.is_finite() {
            fv.wind_dir_deg = dir;
        }
    }
}

/// Parse a dedicated extras-only SimConnect packet (keys match registration order).
pub fn parse_extra_elems(elem: &[f64], keys: &[String]) -> HashMap<String, f64> {
    let mut extras = HashMap::new();
    for (i, key) in keys.iter().enumerate() {
        let v = elem.get(i).copied().unwrap_or(0.0);
        if v.is_finite() {
            extras.insert(key.clone(), v);
        }
    }
    extras
}

pub fn merge_extras(fv: &mut FlightVars, extras: &HashMap<String, f64>) {
    for (key, value) in extras {
        if value.is_finite() {
            fv.extras.insert(key.clone(), *value);
        }
    }
    sync_aircraft_meta(fv);
    sanitize_wind_fields(fv);
}

/// Physical RPM from sim: rated × percent, N2/N1 fallbacks, then sane `GENERAL ENG RPM`.
pub fn sync_eng_rpm(fv: &mut FlightVars) {
    let mut best = 0.0_f64;
    for idx in 1..=2u32 {
        if let Some(rpm) = engine_rpm_from_index(&fv.extras, idx) {
            best = best.max(rpm);
        }
    }
    if best > 0.0 {
        fv.eng_rpm = best;
    }
}

/// MSFS may report percent as 0..1, 0..100, or 0..16384.
fn normalize_sim_percent(value: f64) -> f64 {
    if !value.is_finite() || value < 0.0 {
        return 0.0;
    }
    if value <= 1.5 {
        value * 100.0
    } else if value > 200.0 && value <= 16384.0 {
        value / 163.84
    } else {
        value
    }
}

fn rated_rpm_for_index(extras: &HashMap<String, f64>, index: u32) -> Option<f64> {
    let key = format!("eng_max_rated_rpm_{index}");
    extras
        .get(&key)
        .copied()
        .filter(|r| r.is_finite() && *r > 500.0)
        .map(|r| r.clamp(100.0, 15_000.0))
}

fn rpm_from_rated_and_pct(rated: f64, pct: f64) -> Option<f64> {
    const MIN_RPM: f64 = 40.0;
    const MAX_DISPLAY_RPM: f64 = 9_500.0;

    let pct = normalize_sim_percent(pct);
    if pct < 0.0 || pct > 150.0 {
        return None;
    }
    let rpm = rated * pct / 100.0;
    if rpm >= MIN_RPM && rpm <= MAX_DISPLAY_RPM {
        Some(rpm)
    } else {
        None
    }
}

fn engine_rpm_from_index(extras: &HashMap<String, f64>, index: u32) -> Option<f64> {
    const MIN_RPM: f64 = 40.0;
    const MAX_SANE_RAW_RPM: f64 = 8_500.0;
    const DEFAULT_JET_RATED_RPM: f64 = 5_200.0;

    let pct_key = format!("eng_pct_max_rpm_{index}");
    let raw_key = format!("eng_rpm_{index}");
    let n1_key = format!("eng_n1_{index}");
    let n2_key = format!("eng_n2_{index}");

    let rated = rated_rpm_for_index(extras, index);
    let raw = extras.get(&raw_key).copied().filter(|v| v.is_finite());
    let pct = extras.get(&pct_key).copied();
    let n1 = extras.get(&n1_key).map(|v| normalize_sim_percent(*v));
    let n2 = extras.get(&n2_key).map(|v| normalize_sim_percent(*v));

    let computed_from_pct = rated
        .zip(pct)
        .and_then(|(r, p)| rpm_from_rated_and_pct(r, p));

    let computed_from_n2_rated = rated.zip(n2).and_then(|(r, n2)| {
        if n2 >= 5.0 {
            rpm_from_rated_and_pct(r, n2)
        } else {
            None
        }
    });

    let computed_from_n2_default = if computed_from_n2_rated.is_none() {
        n2.filter(|&n2| n2 >= 5.0).and_then(|n2| {
            rpm_from_rated_and_pct(DEFAULT_JET_RATED_RPM, n2)
        })
    } else {
        None
    };

    let computed_from_n1 = rated.zip(n1).and_then(|(r, n1)| {
        if n1 >= 15.0 {
            rpm_from_rated_and_pct(r, n1)
        } else {
            None
        }
    });

    let computed = computed_from_pct
        .or(computed_from_n2_rated)
        .or(computed_from_n2_default)
        .or(computed_from_n1);

    let sane_raw = |rpm: f64| rpm >= MIN_RPM && rpm <= MAX_SANE_RAW_RPM;

    match (raw, computed) {
        (Some(r), Some(c)) if sane_raw(r) && sane_raw(c) => {
            if r > c * 1.8 {
                Some(c)
            } else {
                Some(r)
            }
        }
        (_, Some(c)) => Some(c),
        (Some(r), _) if sane_raw(r) => Some(r),
        _ => None,
    }
}

fn throttle_norm_from_extras(extras: &HashMap<String, f64>) -> f64 {
    let mut best = 0.0_f64;
    for idx in 1..=2u32 {
        let key = format!("eng_throttle_{idx}");
        if let Some(&t) = extras.get(&key) {
            if t.is_finite() && t >= 0.0 {
                best = best.max((t / 100.0).clamp(0.0, 1.0));
            }
        }
    }
    best
}

fn n1_norm_from_extras(extras: &HashMap<String, f64>, cfg: &RumbleConfig) -> f64 {
    let idle = cfg.engine_idle_n1_pct as f64 / 100.0;
    let mut best = 0.0_f64;
    for idx in 1..=2u32 {
        let key = format!("eng_n1_{idx}");
        if let Some(&n1) = extras.get(&key) {
            if n1.is_finite() && n1 > 1.0 {
                let frac = (n1 / 100.0).clamp(0.0, 1.0);
                if frac > idle * 0.85 {
                    let norm = ((frac - idle) / (1.0 - idle).max(0.08)).clamp(0.0, 1.0);
                    best = best.max(norm);
                }
            }
        }
    }
    best
}

/// Normalized engine power 0 (idle) .. 1 (max), from RPM, throttle, and N1 when available.
pub fn engine_power_norm(fv: &FlightVars, cfg: &RumbleConfig) -> f64 {
    let rpm_norm = rpm_thrust_norm(fv.eng_rpm, cfg);
    let throttle_norm = throttle_norm_from_extras(&fv.extras);
    let n1_norm = n1_norm_from_extras(&fv.extras, cfg);
    rpm_norm.max(throttle_norm).max(n1_norm)
}

/// Turbine aircraft: throttle leads the vibe; N1 confirms spool; RPM is a weak fallback.
pub fn jet_vibe_drive(fv: &FlightVars, cfg: &RumbleConfig) -> f64 {
    let throttle = throttle_norm_from_extras(&fv.extras);
    let n1 = n1_norm_from_extras(&fv.extras, cfg);
    let rpm = rpm_thrust_norm(fv.eng_rpm, cfg) * 0.55;
    throttle.max(n1).max(rpm)
}

pub fn rpm_thrust_norm(rpm: f64, cfg: &RumbleConfig) -> f64 {
    let idle = cfg.eng_rpm_idle as f64;
    let max = cfg.eng_rpm_max as f64;
    if max <= idle {
        return 0.0;
    }
    ((rpm - idle) / (max - idle)).clamp(0.0, 1.0)
}

/// MSFS often reports PAUSED=true while stationary even during active flight.
const ENGINE_RUNNING_RPM: f64 = 40.0;

/// Pause gating uses the PAUSED simvar only. MSFS Pause / Pause_EX1 events are unreliable
/// (Pause_EX1 sets SYSTEM_READY while running; Pause can stick after menus).
fn effective_paused(paused_var: bool, fv: &FlightVars) -> bool {
    if !paused_var {
        return false;
    }
    // Aircraft is moving — sim is clearly not menu-paused.
    if fv.ground_speed_kt > 3.0 || fv.airspeed_indicated > 8.0 {
        return false;
    }
    // Parked with engine turning — sim is live; do not mute engine rumble.
    if fv.on_ground && fv.eng_rpm >= ENGINE_RUNNING_RPM {
        return false;
    }
    true
}

pub fn sanitize_flight_vars(fv: &mut FlightVars, ias_deadband_kn: f64) {
    if !fv.airspeed_indicated.is_finite()
        || fv.airspeed_indicated < -5.0
        || fv.airspeed_indicated > 1200.0
    {
        fv.airspeed_indicated = 0.0;
    }
    if fv.airspeed_indicated.abs() < ias_deadband_kn {
        fv.airspeed_indicated = 0.0;
    }
    if !fv.bank_deg.is_finite() {
        fv.bank_deg = 0.0;
    }
    sanitize_wind_fields(fv);
}

fn sanitize_wind_fields(fv: &mut FlightVars) {
    if !fv.wind_kt.is_finite() || fv.wind_kt < 0.0 {
        fv.wind_kt = 0.0;
    } else if fv.wind_kt > 80.0 {
        fv.wind_kt = 80.0;
    }
    if !fv.wind_dir_deg.is_finite() {
        fv.wind_dir_deg = 0.0;
    } else {
        fv.wind_dir_deg = fv.wind_dir_deg.rem_euclid(360.0);
    }
}

pub fn flight_status(fv: &FlightVars) -> SimStatus {
    if !fv.on_ground && fv.airspeed_indicated > 30.0 {
        SimStatus::InFlight
    } else {
        SimStatus::Connected
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preset::SimVarLayout;

    fn sample_elems() -> [f64; 8] {
        [
            120.0, // IAS
            0.0,   // on ground
            15.0,  // bank
            50.0,  // flaps L
            70.0,  // flaps R
            2.0,   // flaps index
            100.0, // sim time
            0.0,   // paused var
        ]
    }

    fn sample_motion_extras() -> HashMap<String, f64> {
        HashMap::from([
            ("ground_speed_kt".to_string(), 25.0),
            ("gear_handle_bool".to_string(), 1.0),
            ("stall_warning".to_string(), 0.0),
        ])
    }

    fn wind_extras(kt: f64, dir: f64) -> HashMap<String, f64> {
        let mut m = HashMap::new();
        m.insert("wind_kt".to_string(), kt);
        m.insert("wind_dir_deg".to_string(), dir);
        m
    }

    fn parse_with_wind(elems: &[f64], kt: f64, dir: f64) -> FlightVars {
        let layout = SimVarLayout::core_only();
        let mut fv = parse_main_elems(elems, &layout, false, 1.0);
        merge_extras(&mut fv, &wind_extras(kt, dir));
        fv
    }

    #[test]
    fn parses_all_fields_from_core_array() {
        let layout = SimVarLayout::core_only();
        let mut fv = parse_main_elems(&sample_elems(), &layout, false, 1.0);
        let mut extras = sample_motion_extras();
        extras.insert("vertical_speed_fpm".to_string(), -200.0);
        merge_extras(&mut fv, &extras);
        assert_eq!(fv.airspeed_indicated, 120.0);
        assert!(!fv.on_ground);
        assert_eq!(fv.bank_deg, 15.0);
        assert_eq!(fv.flaps_pct, 60.0);
        assert_eq!(fv.flaps_index, 2);
        assert_eq!(fv.gear_handle, 1.0);
        assert!(!fv.stalled);
        assert_eq!(fv.sim_time_s, 100.0);
        assert_eq!(fv.ground_speed_kt, 25.0);
        assert!((fv.vertical_speed_fpm - (-200.0)).abs() < 0.1);
        assert!(!fv.paused);

        let fv_wind = parse_with_wind(&sample_elems(), 18.0, 270.0);
        assert!((fv_wind.wind_kt - 18.0).abs() < 0.1);
        assert!((fv_wind.wind_dir_deg - 270.0).abs() < 0.1);
    }

    #[test]
    fn flaps_pct_is_average_of_left_and_right() {
        let layout = SimVarLayout::core_only();
        let mut e = sample_elems();
        e[3] = 0.0;
        e[4] = 100.0;
        let fv = parse_main_elems(&e, &layout, false, 1.0);
        assert_eq!(fv.flaps_pct, 50.0);
    }

    #[test]
    fn non_finite_ias_becomes_zero() {
        let layout = SimVarLayout::core_only();
        let mut e = sample_elems();
        e[0] = f64::NAN;
        let fv = parse_main_elems(&e, &layout, false, 1.0);
        assert_eq!(fv.airspeed_indicated, 0.0);
    }

    #[test]
    fn out_of_range_ias_becomes_zero() {
        let layout = SimVarLayout::core_only();
        let mut e = sample_elems();
        e[0] = 1500.0;
        let fv = parse_main_elems(&e, &layout, false, 1.0);
        assert_eq!(fv.airspeed_indicated, 0.0);
    }

    #[test]
    fn ias_within_deadband_becomes_zero() {
        let layout = SimVarLayout::core_only();
        let mut e = sample_elems();
        e[0] = 0.5;
        let fv = parse_main_elems(&e, &layout, false, 1.0);
        assert_eq!(fv.airspeed_indicated, 0.0);
    }

    #[test]
    fn pause_from_simvar_only_events_ignored() {
        let layout = SimVarLayout::core_only();
        let e = sample_elems();
        let from_events = parse_main_elems(&e, &layout, true, 1.0);
        assert!(!from_events.paused);

        let mut paused_elem = e;
        paused_elem[7] = 1.0;
        paused_elem[0] = 0.0;
        paused_elem[1] = 1.0;
        let parked = parse_main_elems(&paused_elem, &layout, false, 1.0);
        assert!(parked.paused);

        let mut running = parked.clone();
        running.eng_rpm = 1405.0;
        finalize_flight_vars(&mut running);
        assert!(
            !running.paused,
            "parked engine running must not block rumble"
        );

        paused_elem[0] = 120.0;
        let mut moving = parse_main_elems(&paused_elem, &layout, false, 1.0);
        merge_extras(
            &mut moving,
            &HashMap::from([("ground_speed_kt".to_string(), 40.0)]),
        );
        finalize_flight_vars(&mut moving);
        assert!(!moving.paused);
    }

    #[test]
    fn non_finite_bank_becomes_zero() {
        let layout = SimVarLayout::core_only();
        let mut e = sample_elems();
        e[2] = f64::INFINITY;
        let fv = parse_main_elems(&e, &layout, false, 1.0);
        assert_eq!(fv.bank_deg, 0.0);
    }

    #[test]
    fn ground_speed_is_clamped_to_non_negative() {
        let mut fv = FlightVars::default();
        fv.extras.insert("ground_speed_kt".to_string(), -5.0);
        sync_aircraft_meta(&mut fv);
        assert_eq!(fv.ground_speed_kt, 0.0);
    }

    #[test]
    fn stall_warning_ignored_below_ias_threshold() {
        let mut fv = FlightVars {
            airspeed_indicated: 20.0,
            extras: HashMap::from([("stall_warning".to_string(), 1.0)]),
            ..Default::default()
        };
        sync_aircraft_meta(&mut fv);
        assert!(!fv.stalled);
    }

    #[test]
    fn stall_warning_active_above_ias_threshold() {
        let mut fv = FlightVars {
            airspeed_indicated: 80.0,
            extras: HashMap::from([("stall_warning".to_string(), 1.0)]),
            ..Default::default()
        };
        sync_aircraft_meta(&mut fv);
        assert!(fv.stalled);
    }

    #[test]
    fn non_finite_wind_becomes_zero() {
        let mut extras = HashMap::new();
        extras.insert("wind_kt".to_string(), f64::NAN);
        extras.insert("wind_dir_deg".to_string(), 270.0);
        let layout = SimVarLayout::core_only();
        let mut fv = parse_main_elems(&sample_elems(), &layout, false, 1.0);
        merge_extras(&mut fv, &extras);
        assert_eq!(fv.wind_kt, 0.0);
    }

    #[test]
    fn wind_clamped_to_sanity_max() {
        let fv = parse_with_wind(&sample_elems(), 160.0, 90.0);
        assert_eq!(fv.wind_kt, 80.0);
    }

    #[test]
    fn wind_speed_and_direction_are_independent() {
        let fv = parse_with_wind(&sample_elems(), 12.0, 90.0);
        assert_eq!(fv.wind_kt, 12.0);
        assert_eq!(fv.wind_dir_deg, 90.0);
    }

    #[test]
    fn flight_status_in_flight_when_airborne_and_fast() {
        let layout = SimVarLayout::core_only();
        let mut e = sample_elems();
        e[0] = 150.0;
        e[1] = 0.0;
        let fv = parse_main_elems(&e, &layout, false, 1.0);
        assert_eq!(flight_status(&fv), SimStatus::InFlight);
    }

    #[test]
    fn flight_status_connected_on_ground() {
        let layout = SimVarLayout::core_only();
        let mut e = sample_elems();
        e[1] = 1.0;
        e[0] = 150.0;
        let fv = parse_main_elems(&e, &layout, false, 1.0);
        assert_eq!(flight_status(&fv), SimStatus::Connected);
    }

    #[test]
    fn flight_status_connected_when_slow_airborne() {
        let layout = SimVarLayout::core_only();
        let mut e = sample_elems();
        e[0] = 20.0;
        e[1] = 0.0;
        let fv = parse_main_elems(&e, &layout, false, 1.0);
        assert_eq!(flight_status(&fv), SimStatus::Connected);
    }

    #[test]
    fn extra_simvar_populates_extras_map() {
        let layout = SimVarLayout::core_only().with_extra_keys(vec!["spoilers_pct".to_string()]);
        let mut e: Vec<f64> = sample_elems().to_vec();
        e.push(75.0);
        let fv = parse_main_elems(&e, &layout, false, 1.0);
        assert_eq!(fv.extras.get("spoilers_pct"), Some(&75.0));
    }

    #[test]
    fn extra_elems_populate_extras_map() {
        let keys = vec!["eng_rpm_1".to_string(), "eng_throttle_1".to_string()];
        let extras = parse_extra_elems(&[2400.0, 75.0], &keys);
        assert_eq!(extras.get("eng_rpm_1"), Some(&2400.0));
        assert_eq!(extras.get("eng_throttle_1"), Some(&75.0));
    }

    #[test]
    fn skips_unregistered_core_fields_without_shifting_extras() {
        let mut layout = SimVarLayout::core_only();
        layout.fields.pop(); // paused var not registered
        layout
            .fields
            .push(LayoutField::Extra("eng_rpm_1".to_string()));

        let mut e = sample_elems().to_vec();
        e.pop();
        e.push(2400.0);

        let fv = parse_main_elems(&e, &layout, false, 1.0);
        assert_eq!(fv.wind_kt, 0.0);
        assert_eq!(fv.extras.get("eng_rpm_1"), Some(&2400.0));
        assert_eq!(fv.eng_rpm, 2400.0);
    }

    #[test]
    fn sync_eng_rpm_uses_max_rated_times_pct() {
        let mut fv = FlightVars::default();
        fv.extras.insert("eng_max_rated_rpm_1".to_string(), 5200.0);
        fv.extras.insert("eng_pct_max_rpm_1".to_string(), 104.0);
        fv.extras.insert("eng_rpm_1".to_string(), 28_000.0);
        sync_aircraft_meta(&mut fv);
        assert!((fv.eng_rpm - 5408.0).abs() < 1.0);
    }

    #[test]
    fn sync_eng_rpm_uses_max_of_twin_engines() {
        let mut fv = FlightVars::default();
        fv.extras.insert("eng_max_rated_rpm_1".to_string(), 5200.0);
        fv.extras.insert("eng_pct_max_rpm_1".to_string(), 60.0);
        fv.extras.insert("eng_max_rated_rpm_2".to_string(), 5200.0);
        fv.extras.insert("eng_pct_max_rpm_2".to_string(), 95.0);
        sync_aircraft_meta(&mut fv);
        assert!((fv.eng_rpm - 4940.0).abs() < 1.0);
    }

    #[test]
    fn sync_eng_rpm_rejects_insane_rated_rpm() {
        let mut fv = FlightVars::default();
        fv.extras.insert("eng_max_rated_rpm_1".to_string(), 27_000.0);
        fv.extras.insert("eng_pct_max_rpm_1".to_string(), 80.0);
        fv.extras.insert("eng_rpm_1".to_string(), 2_400.0);
        sync_aircraft_meta(&mut fv);
        assert!((fv.eng_rpm - 2400.0).abs() < 1.0);
    }

    #[test]
    fn sync_eng_rpm_uses_n2_when_rated_is_insane() {
        let mut fv = FlightVars::default();
        fv.extras.insert("eng_max_rated_rpm_1".to_string(), 27_000.0);
        fv.extras.insert("eng_pct_max_rpm_1".to_string(), 0.8);
        fv.extras.insert("eng_rpm_1".to_string(), 28_000.0);
        fv.extras.insert("eng_n2_1".to_string(), 85.0);
        sync_aircraft_meta(&mut fv);
        assert!((fv.eng_rpm - 4420.0).abs() < 1.0);
    }

    #[test]
    fn sync_eng_rpm_accepts_pct_zero_to_one_scale() {
        let mut fv = FlightVars::default();
        fv.extras.insert("eng_max_rated_rpm_1".to_string(), 5200.0);
        fv.extras.insert("eng_pct_max_rpm_1".to_string(), 0.85);
        sync_aircraft_meta(&mut fv);
        assert!((fv.eng_rpm - 4420.0).abs() < 1.0);
    }

    #[test]
    fn vertical_speed_from_extras() {
        let mut fv = FlightVars::default();
        fv.extras.insert("vertical_speed_fpm".to_string(), -1200.0);
        sync_aircraft_meta(&mut fv);
        assert!((fv.vertical_speed_fpm - -1200.0).abs() < 0.1);
    }

    #[test]
    fn sync_eng_rpm_ignores_inflated_animation_rpm() {
        let mut fv = FlightVars::default();
        fv.extras.insert("eng_max_rated_rpm_1".to_string(), 5200.0);
        fv.extras.insert("eng_pct_max_rpm_1".to_string(), 60.0);
        fv.extras.insert("eng_rpm_1".to_string(), 28_000.0);
        sync_aircraft_meta(&mut fv);
        assert!((fv.eng_rpm - 3120.0).abs() < 1.0);
    }

    #[test]
    fn jet_vibe_drive_follows_throttle() {
        let fv = FlightVars {
            eng_rpm: 2500.0,
            extras: HashMap::from([
                ("eng_n1_1".to_string(), 22.0),
                ("eng_throttle_1".to_string(), 85.0),
            ]),
            ..Default::default()
        };
        let cfg = RumbleConfig {
            eng_rpm_idle: 2500.0,
            eng_rpm_max: 5200.0,
            engine_idle_n1_pct: 22.0,
            ..RumbleConfig::default()
        };
        let drive = jet_vibe_drive(&fv, &cfg);
        assert!(drive > 0.8, "throttle should drive jet vibe, got {drive}");
    }

    #[test]
    fn engine_power_norm_uses_throttle_for_spool() {
        let fv = FlightVars {
            eng_rpm: 1000.0,
            extras: HashMap::from([("eng_throttle_1".to_string(), 85.0)]),
            ..Default::default()
        };
        let cfg = RumbleConfig {
            eng_rpm_idle: 1000.0,
            eng_rpm_max: 2550.0,
            ..RumbleConfig::default()
        };
        let norm = engine_power_norm(&fv, &cfg);
        assert!(norm > 0.8, "throttle should dominate at idle RPM, got {norm}");
    }

    #[test]
    fn engine_power_norm_prefers_pct_max_rpm() {
        let fv = FlightVars {
            eng_rpm: 4000.0,
            ..Default::default()
        };
        let cfg = RumbleConfig {
            eng_rpm_idle: 2500.0,
            eng_rpm_max: 5200.0,
            ..RumbleConfig::default()
        };
        let norm = engine_power_norm(&fv, &cfg);
        assert!(norm > 0.5 && norm < 0.7, "got {norm}");
    }

    #[test]
    fn merge_extras_sets_eng_rpm_field() {
        let mut fv = FlightVars::default();
        let mut extras = HashMap::new();
        extras.insert("eng_rpm_1".to_string(), 1050.0);
        merge_extras(&mut fv, &extras);
        assert_eq!(fv.eng_rpm, 1050.0);
    }
}
