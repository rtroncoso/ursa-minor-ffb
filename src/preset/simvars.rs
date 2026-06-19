//! Canonical SimConnect simvar names and units (MSFS SDK spelling).
//! Runtime always uses these definitions from code — YAML only stores rumble sliders.

use super::{PresetKind, SimVarDef, SimVarProfile, SIMCONNECT_UNUSED_DATUM};

pub const OBSOLETE_EXTRA_KEYS: &[&str] = &["recip_mag_l", "recip_mag_r", "prop_rpm_1"];

/// Core simvars registered for every preset (order is the SimConnect packet layout).
/// Wind lives in the extras packet so speed/direction cannot shift when a core field fails to register.
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
    ("VERTICAL SPEED", "Feet per minute"),
];

pub const CORE_SIMVAR_COUNT: usize = CORE_SIMVARS.len();

pub fn canonical_extras_for(kind: PresetKind) -> SimVarProfile {
    let mut simvars = SimVarProfile::default();
    push_spoilers_extra(&mut simvars);
    push_wind_extras(&mut simvars);
    match kind {
        PresetKind::GeneralAviation => {
            push_aircraft_engine_extras(&mut simvars, true, false);
        }
        PresetKind::Commercial => {
            push_aircraft_engine_extras(&mut simvars, true, true);
            push_extra(&mut simvars, "TURB ENG N2", "Percent", "eng_n2_1", 1);
        }
        PresetKind::Fighter => {
            push_aircraft_engine_extras(&mut simvars, false, true);
        }
        PresetKind::Custom => {
            return canonical_extras_for(PresetKind::Commercial);
        }
    }
    simvars
}

fn push_wind_extras(simvars: &mut SimVarProfile) {
    push_extra(
        simvars,
        "AMBIENT WIND VELOCITY",
        "Knots",
        "wind_kt",
        SIMCONNECT_UNUSED_DATUM,
    );
    push_extra(
        simvars,
        "AMBIENT WIND DIRECTION",
        "Degrees",
        "wind_dir_deg",
        SIMCONNECT_UNUSED_DATUM,
    );
}

fn push_spoilers_extra(simvars: &mut SimVarProfile) {
    push_extra(
        simvars,
        "SPOILERS HANDLE POSITION",
        "Percent",
        "spoilers_pct",
        SIMCONNECT_UNUSED_DATUM,
    );
}

/// Engine simvars shared across aircraft types (piston, turboprop, jet, twin).
fn push_aircraft_engine_extras(simvars: &mut SimVarProfile, twin: bool, turbine_gauges: bool) {
    push_extra(
        simvars,
        "NUMBER OF ENGINES",
        "Number",
        "num_engines",
        SIMCONNECT_UNUSED_DATUM,
    );
    push_extra(
        simvars,
        "MAX RATED ENGINE RPM",
        "Rpm",
        "eng_max_rated_rpm_1",
        1,
    );
    push_extra(
        simvars,
        "GENERAL ENG PCT MAX RPM",
        "Percent",
        "eng_pct_max_rpm_1",
        1,
    );
    push_extra(simvars, "GENERAL ENG RPM", "Rpm", "eng_rpm_1", 1);
    if twin {
        push_extra(simvars, "MAX RATED ENGINE RPM", "Rpm", "eng_max_rated_rpm_2", 2);
        push_extra(
            simvars,
            "GENERAL ENG PCT MAX RPM",
            "Percent",
            "eng_pct_max_rpm_2",
            2,
        );
        push_extra(simvars, "GENERAL ENG RPM", "Rpm", "eng_rpm_2", 2);
    }
    if turbine_gauges {
        push_extra(simvars, "TURB ENG N1", "Percent", "eng_n1_1", 1);
    }
    push_extra(
        simvars,
        "GENERAL ENG THROTTLE LEVER POSITION",
        "Percent",
        "eng_throttle_1",
        1,
    );
}

fn push_extra(simvars: &mut SimVarProfile, name: &str, unit: &str, key: &str, datum_index: u32) {
    simvars.extra.push(SimVarDef {
        name: name.to_string(),
        unit: unit.to_string(),
        key: key.to_string(),
        datum_index,
    });
}

impl SimVarProfile {
    /// Replace extras with the canonical built-in list (order, names, units). User rumble sliders are untouched.
    pub fn apply_canonical_extras(&mut self, canonical: &SimVarProfile) {
        self.extra = canonical.extra.clone();
        self.normalize();
    }

    pub fn strip_obsolete_extras(&mut self) {
        self.extra
            .retain(|d| !OBSOLETE_EXTRA_KEYS.contains(&d.key.as_str()));
        self.normalize();
    }
}
