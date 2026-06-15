use crate::{FlightVars, SimStatus};

pub fn parse_main_elems(
    elem: &[f64],
    paused_from_events: bool,
    ias_deadband_kn: f64,
) -> FlightVars {
    let paused_from_var = elem.get(10).copied().unwrap_or(0.0) != 0.0;

    let mut fv = FlightVars {
        airspeed_indicated: elem.get(0).copied().unwrap_or(0.0),
        on_ground: elem.get(1).copied().unwrap_or(0.0) != 0.0,
        bank_deg: elem.get(2).copied().unwrap_or(0.0),
        flaps_pct: ((elem.get(3).copied().unwrap_or(0.0) + elem.get(4).copied().unwrap_or(0.0))
            * 0.5)
            .clamp(0.0, 100.0),
        flaps_index: elem.get(5).copied().unwrap_or(0.0).round() as i32,
        gear_handle: elem.get(6).copied().unwrap_or(0.0),
        stalled: elem.get(7).copied().unwrap_or(0.0) != 0.0,
        sim_time_s: elem.get(8).copied().unwrap_or(0.0),
        ground_speed_kt: elem.get(9).copied().unwrap_or(0.0).max(0.0),
        paused: paused_from_events || paused_from_var,
    };

    sanitize_flight_vars(&mut fv, ias_deadband_kn);
    fv
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

    fn sample_elems() -> [f64; 11] {
        [
            120.0, // IAS
            0.0,   // on ground
            15.0,  // bank
            50.0,  // flaps L
            70.0,  // flaps R
            2.0,   // flaps index
            1.0,   // gear
            0.0,   // stall
            100.0, // sim time
            25.0,  // ground speed
            0.0,   // paused var
        ]
    }

    #[test]
    fn parses_all_fields_from_eleven_element_array() {
        let fv = parse_main_elems(&sample_elems(), false, 1.0);
        assert_eq!(fv.airspeed_indicated, 120.0);
        assert!(!fv.on_ground);
        assert_eq!(fv.bank_deg, 15.0);
        assert_eq!(fv.flaps_pct, 60.0);
        assert_eq!(fv.flaps_index, 2);
        assert_eq!(fv.gear_handle, 1.0);
        assert!(!fv.stalled);
        assert_eq!(fv.sim_time_s, 100.0);
        assert_eq!(fv.ground_speed_kt, 25.0);
        assert!(!fv.paused);
    }

    #[test]
    fn flaps_pct_is_average_of_left_and_right() {
        let mut e = sample_elems();
        e[3] = 0.0;
        e[4] = 100.0;
        let fv = parse_main_elems(&e, false, 1.0);
        assert_eq!(fv.flaps_pct, 50.0);
    }

    #[test]
    fn non_finite_ias_becomes_zero() {
        let mut e = sample_elems();
        e[0] = f64::NAN;
        let fv = parse_main_elems(&e, false, 1.0);
        assert_eq!(fv.airspeed_indicated, 0.0);
    }

    #[test]
    fn out_of_range_ias_becomes_zero() {
        let mut e = sample_elems();
        e[0] = 1500.0;
        let fv = parse_main_elems(&e, false, 1.0);
        assert_eq!(fv.airspeed_indicated, 0.0);
    }

    #[test]
    fn ias_within_deadband_becomes_zero() {
        let mut e = sample_elems();
        e[0] = 0.5;
        let fv = parse_main_elems(&e, false, 1.0);
        assert_eq!(fv.airspeed_indicated, 0.0);
    }

    #[test]
    fn pause_from_events_or_elem() {
        let e = sample_elems();
        let from_events = parse_main_elems(&e, true, 1.0);
        assert!(from_events.paused);

        let mut paused_elem = e;
        paused_elem[10] = 1.0;
        let from_var = parse_main_elems(&paused_elem, false, 1.0);
        assert!(from_var.paused);
    }

    #[test]
    fn non_finite_bank_becomes_zero() {
        let mut e = sample_elems();
        e[2] = f64::INFINITY;
        let fv = parse_main_elems(&e, false, 1.0);
        assert_eq!(fv.bank_deg, 0.0);
    }

    #[test]
    fn ground_speed_is_clamped_to_non_negative() {
        let mut e = sample_elems();
        e[9] = -5.0;
        let fv = parse_main_elems(&e, false, 1.0);
        assert_eq!(fv.ground_speed_kt, 0.0);
    }

    #[test]
    fn flight_status_in_flight_when_airborne_and_fast() {
        let mut e = sample_elems();
        e[0] = 150.0;
        e[1] = 0.0;
        let fv = parse_main_elems(&e, false, 1.0);
        assert_eq!(flight_status(&fv), SimStatus::InFlight);
    }

    #[test]
    fn flight_status_connected_on_ground() {
        let mut e = sample_elems();
        e[1] = 1.0;
        e[0] = 150.0;
        let fv = parse_main_elems(&e, false, 1.0);
        assert_eq!(flight_status(&fv), SimStatus::Connected);
    }

    #[test]
    fn flight_status_connected_when_slow_airborne() {
        let mut e = sample_elems();
        e[0] = 20.0;
        e[1] = 0.0;
        let fv = parse_main_elems(&e, false, 1.0);
        assert_eq!(flight_status(&fv), SimStatus::Connected);
    }
}
