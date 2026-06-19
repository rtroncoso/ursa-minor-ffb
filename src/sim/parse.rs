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
            LayoutField::VerticalSpeed => fv.vertical_speed_fpm = v,
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
    if let Some(n) = fv.extras.get("num_engines").copied() {
        if n.is_finite() && n >= 0.0 {
            fv.num_engines = n.round().max(0.0) as u32;
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

/// Physical RPM from sim: `MAX RATED ENGINE RPM × GENERAL ENG PCT MAX RPM / 100`,
/// falling back to `GENERAL ENG RPM` when it is in a sane range (piston / turboprop).
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

fn engine_rpm_from_index(extras: &HashMap<String, f64>, index: u32) -> Option<f64> {
    let rated_key = format!("eng_max_rated_rpm_{index}");
    let pct_key = format!("eng_pct_max_rpm_{index}");
    let raw_key = format!("eng_rpm_{index}");

    if let (Some(&rated), Some(&pct)) = (extras.get(&rated_key), extras.get(&pct_key)) {
        if rated > 100.0 && pct.is_finite() && pct >= 0.0 {
            return Some(rated * pct / 100.0);
        }
    }
    extras
        .get(&raw_key)
        .copied()
        .filter(|&raw| raw.is_finite() && (40.0..=12_000.0).contains(&raw))
}

/// Normalized engine power 0 (idle) .. 1 (max), from resolved physical RPM.
pub fn engine_power_norm(fv: &FlightVars, cfg: &RumbleConfig) -> f64 {
    rpm_thrust_norm(fv.eng_rpm, cfg)
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

    fn sample_elems() -> [f64; 12] {
        [
            120.0,  // IAS
            0.0,    // on ground
            15.0,   // bank
            50.0,   // flaps L
            70.0,   // flaps R
            2.0,    // flaps index
            1.0,    // gear
            0.0,    // stall
            100.0,  // sim time
            25.0,   // ground speed
            0.0,    // paused var
            -200.0, // vertical speed (fpm)
        ]
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
    fn parses_all_fields_from_eleven_element_array() {
        let layout = SimVarLayout::core_only();
        let fv = parse_main_elems(&sample_elems(), &layout, false, 1.0);
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
        paused_elem[10] = 1.0;
        paused_elem[0] = 0.0;
        paused_elem[1] = 1.0;
        paused_elem[9] = 0.0;
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
        paused_elem[9] = 40.0;
        let mut moving = parse_main_elems(&paused_elem, &layout, false, 1.0);
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
        let layout = SimVarLayout::core_only();
        let mut e = sample_elems();
        e[9] = -5.0;
        let fv = parse_main_elems(&e, &layout, false, 1.0);
        assert_eq!(fv.ground_speed_kt, 0.0);
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
        layout.fields.pop(); // vertical speed not registered
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
