//! Canonical SimConnect simvar names and units (MSFS SDK spelling).
//! Runtime always uses these definitions from code — YAML only stores rumble sliders.

use super::{PresetKind, SimVarDef, SimVarProfile, SIMCONNECT_UNUSED_DATUM};

pub const OBSOLETE_EXTRA_KEYS: &[&str] = &["recip_mag_l", "recip_mag_r", "prop_rpm_1"];

/// Core simvars registered for every preset (order is the SimConnect packet layout).
pub const CORE_SIMVARS: &[(&str, &str)] = &[
    ("AIRSPEED INDICATED", "knots"),
    ("SIM ON GROUND", "bool"),
    ("PLANE BANK DEGREES", "degrees"),
    ("TRAILING EDGE FLAPS LEFT PERCENT", "percent"),
    ("TRAILING EDGE FLAPS RIGHT PERCENT", "percent"),
    ("FLAPS HANDLE INDEX", "number"),
    ("GEAR HANDLE POSITION", "bool"),
    ("STALL WARNING", "bool"),
    ("ABSOLUTE TIME", "seconds"),
    ("GROUND VELOCITY", "knots"),
    ("PAUSED", "bool"),
    ("AMBIENT WIND VELOCITY", "knots"),
    ("AMBIENT WIND DIRECTION", "degrees"),
];

pub const CORE_SIMVAR_COUNT: usize = CORE_SIMVARS.len();

pub fn canonical_extras_for(kind: PresetKind) -> SimVarProfile {
    let mut simvars = SimVarProfile::default();
    match kind {
        PresetKind::GeneralAviation => {
            push_aircraft_engine_extras(&mut simvars, true);
        }
        PresetKind::Commercial => {
            push_extra(
                &mut simvars,
                "SPOILERS HANDLE POSITION",
                "percent",
                "spoilers_pct",
                SIMCONNECT_UNUSED_DATUM,
            );
            push_aircraft_engine_extras(&mut simvars, true);
            push_extra(&mut simvars, "TURB ENG N1", "percent", "eng_n1_1", 1);
            push_extra(&mut simvars, "TURB ENG N2", "percent", "eng_n2_1", 1);
        }
        PresetKind::Fighter => {
            push_aircraft_engine_extras(&mut simvars, false);
            push_extra(&mut simvars, "TURB ENG N1", "percent", "eng_n1_1", 1);
        }
        PresetKind::Custom => {
            return canonical_extras_for(PresetKind::Commercial);
        }
    }
    simvars
}

/// Engine simvars shared across aircraft types (single/twin piston, turboprop, helo).
fn push_aircraft_engine_extras(simvars: &mut SimVarProfile, twin_rpm: bool) {
    push_extra(
        simvars,
        "NUMBER OF ENGINES",
        "Number",
        "num_engines",
        SIMCONNECT_UNUSED_DATUM,
    );
    push_extra(simvars, "GENERAL ENG RPM", "Rpm", "eng_rpm_1", 1);
    push_extra(simvars, "GENERAL ENG PCT MAX RPM", "Percent", "eng_pct_max_rpm_1", 1);
    if twin_rpm {
        push_extra(simvars, "GENERAL ENG RPM", "Rpm", "eng_rpm_2", 2);
    }
    push_extra(
        simvars,
        "GENERAL ENG THROTTLE LEVER POSITION",
        "percent",
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
