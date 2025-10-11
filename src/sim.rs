use std::ffi::{c_char, c_void, CStr};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossbeam_channel::Sender;
use libloading::Library;
use parking_lot::Mutex;

use crate::{ui::SimStatus, HidCmd};
use crate::{ConfigShared, EffectsShared, FlightVars, LogBuffer};

// -----------------------------
// SimConnect minimal FFI
// -----------------------------
type DWord = u32;
type HRESULT = i32;
type Handle = *mut c_void;
type HWnd = *mut c_void;

#[repr(C)]
struct SimRecv {
    dw_size: DWord,
    dw_version: DWord,
    dw_id: DWord,
}

#[repr(C)]
struct SimRecvSimObjectData {
    base: SimRecv,
    dw_request_id: DWord,
    dw_object_id: DWord,
    dw_define_id: DWord,
    dw_flags: DWord,
    dw_entrynumber: DWord,
    dw_outof: DWord,
    dw_define_count: DWord,
    dw_data: DWord, // payload starts here
}

#[repr(C)]
struct SimRecvEvent {
    base: SimRecv,
    u_group_id: DWord,
    u_event_id: DWord,
    dw_data: DWord, // for Pause: 1=paused, 0=unpaused; EX1 returns bit flags
}

const SIMCONNECT_RECV_ID_OPEN: DWord = 2;
const SIMCONNECT_RECV_ID_QUIT: DWord = 3;
const SIMCONNECT_RECV_ID_EVENT: DWord = 4;
const SIMCONNECT_RECV_ID_EXCEPTION: DWord = 5;
const SIMCONNECT_RECV_ID_SIMOBJECT_DATA: DWord = 8;

const SIMCONNECT_PERIOD_ONCE: DWord = 1;
const SIMCONNECT_PERIOD_SIM_FRAME: DWord = 3;

const SIMCONNECT_DATATYPE_FLOAT64: DWord = 4;
const SIMCONNECT_DATATYPE_STRING256: DWord = 12;

const USER_OBJECT_ID: DWord = 0;

const EVT_SIM_START: DWord = 1001;
const EVT_SIM_STOP: DWord = 1002;
const EVT_FRAME: DWord = 1003;

// Local IDs for pause subscriptions
const EVT_PAUSE_SYS: DWord = 4101;
const EVT_PAUSE_EX1_SYS: DWord = 4102;

const DEF_MAIN: DWord = 2001;
const REQ_MAIN: DWord = 3001;
const DEF_PING: DWord = 2101;
const REQ_PING: DWord = 3101;
const DEF_TITLE: DWord = 2201;
const REQ_TITLE: DWord = 3201;

type PfnSimConnectOpen =
    unsafe extern "system" fn(*mut Handle, *const c_char, HWnd, DWord, Handle, DWord) -> HRESULT;
type PfnSimConnectClose = unsafe extern "system" fn(Handle) -> HRESULT;
type PfnSimConnectAddToDataDefinition = unsafe extern "system" fn(
    Handle,
    DWord,
    *const c_char,
    *const c_char,
    DWord,
    f32,
    DWord,
) -> HRESULT;
type PfnSimConnectRequestDataOnSimObject = unsafe extern "system" fn(
    Handle,
    DWord,
    DWord,
    DWord,
    DWord,
    DWord,
    DWord,
    DWord,
    DWord,
) -> HRESULT;
type PfnSimConnectGetNextDispatch =
    unsafe extern "system" fn(Handle, *mut *mut SimRecv, *mut DWord) -> HRESULT;
type PfnSimConnectSubscribeToSystemEvent =
    unsafe extern "system" fn(Handle, DWord, *const c_char) -> HRESULT;

#[inline]
fn hr_hex(hr: HRESULT) -> String {
    format!("0x{:08X}", hr as u32)
}

#[derive(Clone)]
struct SimConnectFns {
    _lib: Arc<Library>,
    open: PfnSimConnectOpen,
    close: PfnSimConnectClose,
    add_to_def: PfnSimConnectAddToDataDefinition,
    req_data: PfnSimConnectRequestDataOnSimObject,
    next_dispatch: PfnSimConnectGetNextDispatch,
    subscribe_event: Option<PfnSimConnectSubscribeToSystemEvent>,
}

// ---------- Embedded SimConnect fallback ----------
const EMBED_SIMCONNECT_BYTES: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/lib/SimConnect.dll"));

fn try_load_embedded_simconnect(logs: &LogBuffer) -> Result<Library> {
    let mut dst = std::env::temp_dir();
    dst.push("ursa-simconnect-embedded-64.dll");

    logs.push(format!(
        "SimConnect: writing embedded DLL to {}",
        dst.display()
    ));
    std::fs::write(&dst, EMBED_SIMCONNECT_BYTES)
        .with_context(|| format!("write {}", dst.display()))?;

    logs.push(format!(
        "SimConnect: loading embedded DLL from {}",
        dst.display()
    ));
    let lib = unsafe { Library::new(&dst) }
        .with_context(|| format!("Library::new({})", dst.display()))?;

    logs.push("SimConnect: embedded DLL loaded successfully");
    Ok(lib)
}

fn bind_simconnect(lib: Library) -> Result<SimConnectFns> {
    unsafe {
        let open: PfnSimConnectOpen = *lib.get(b"SimConnect_Open\0")?;
        let close: PfnSimConnectClose = *lib.get(b"SimConnect_Close\0")?;
        let add_to_def: PfnSimConnectAddToDataDefinition =
            *lib.get(b"SimConnect_AddToDataDefinition\0")?;
        let req_data: PfnSimConnectRequestDataOnSimObject =
            *lib.get(b"SimConnect_RequestDataOnSimObject\0")?;
        let next_dispatch: PfnSimConnectGetNextDispatch =
            *lib.get(b"SimConnect_GetNextDispatch\0")?;
        let subscribe_event: Option<PfnSimConnectSubscribeToSystemEvent> = lib
            .get::<PfnSimConnectSubscribeToSystemEvent>(b"SimConnect_SubscribeToSystemEvent\0")
            .ok()
            .map(|s| *s);

        Ok(SimConnectFns {
            _lib: std::sync::Arc::new(lib),
            open,
            close,
            add_to_def,
            req_data,
            next_dispatch,
            subscribe_event,
        })
    }
}

fn load_simconnect(logs: &LogBuffer) -> Result<SimConnectFns> {
    logs.push("SimConnect: trying normal load (EXE dir / PATH)...");
    match unsafe { Library::new("SimConnect.dll") } {
        Ok(lib) => {
            logs.push("SimConnect: loaded via normal search");
            return bind_simconnect(lib);
        }
        Err(e) => {
            logs.push(format!("SimConnect: normal search failed: {e}"));
        }
    }

    let lib = try_load_embedded_simconnect(logs)
        .context("embedded SimConnect fallback was unavailable or failed to load")?;
    bind_simconnect(lib)
}

// -----------------------------
// Sim worker
// -----------------------------
pub fn sim_worker(
    last_vars: Arc<Mutex<Option<FlightVars>>>,
    tx_hid: Sender<HidCmd>,
    logs: LogBuffer,
    config: Arc<ConfigShared>,
    effects: EffectsShared,
    hold: Arc<AtomicBool>,
    status: Arc<Mutex<SimStatus>>,
    aircraft_title: Arc<Mutex<String>>,
) {
    logs.push("SimConnect: worker started");

    let fns = match load_simconnect(&logs) {
        Ok(f) => {
            logs.push("SimConnect: loaded (normal search or embedded fallback)");
            f
        }
        Err(e) => {
            logs.push(format!("SimConnect: {}", e));
            return;
        }
    };

    unsafe {
        loop {
            let mut h_sc: Handle = std::ptr::null_mut();
            let name = std::ffi::CString::new("UrsaMinorFFB").unwrap();
            let hr = (fns.open)(
                &mut h_sc,
                name.as_ptr(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                0xFFFFFFFF,
            );
            if hr < 0 || h_sc.is_null() {
                logs.push(format!("SimConnect: Open failed {}", hr_hex(hr)));
                thread::sleep(Duration::from_millis(1000));
                continue;
            }
            *status.lock() = SimStatus::Connected;
            *aircraft_title.lock() = String::new();

            let mut in_flight: bool = true;

            if let Some(sub) = fns.subscribe_event {
                for (id, ev) in &[
                    (EVT_SIM_START, "SimStart"),
                    (EVT_SIM_STOP, "SimStop"),
                    (EVT_FRAME, "Frame"),
                ] {
                    let ev_c = std::ffi::CString::new(*ev).unwrap();
                    let hr = sub(h_sc, *id, ev_c.as_ptr());
                    if hr < 0 {
                        logs.push(format!(
                            "SimConnect: subscribe {} FAILED {}",
                            ev,
                            hr_hex(hr)
                        ));
                    }
                }

                if let Ok(c) = std::ffi::CString::new("Pause") {
                    let hr = sub(h_sc, EVT_PAUSE_SYS, c.as_ptr());
                    if hr < 0 {
                        logs.push(format!("SimConnect: subscribe Pause FAILED {}", hr_hex(hr)));
                    }
                }
                if let Ok(c) = std::ffi::CString::new("Pause_EX1") {
                    let hr = sub(h_sc, EVT_PAUSE_EX1_SYS, c.as_ptr());
                    if hr < 0 {
                        logs.push(format!(
                            "SimConnect: subscribe Pause_EX1 FAILED {}",
                            hr_hex(hr)
                        ));
                    }
                }
                logs.push("SimConnect: Pause subscriptions active.".to_string());
            }

            let add = |def_id: DWord, name_s: &str, unit_s: &str| -> HRESULT {
                let n = std::ffi::CString::new(name_s).unwrap();
                let u = std::ffi::CString::new(unit_s).unwrap();
                (fns.add_to_def)(
                    h_sc,
                    def_id,
                    n.as_ptr(),
                    u.as_ptr(),
                    SIMCONNECT_DATATYPE_FLOAT64,
                    0.0,
                    0xFFFF_FFFF,
                )
            };

            let defs = [
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
            ];
            for (name, unit) in defs {
                let hr = add(DEF_MAIN, name, unit);
                if hr < 0 {
                    logs.push(format!(
                        "SimConnect: AddToDef {:?} [{}] FAILED {}",
                        name,
                        unit,
                        hr_hex(hr)
                    ));
                }
            }

            {
                let n = std::ffi::CString::new("TITLE").unwrap();
                let hr = (fns.add_to_def)(
                    h_sc,
                    DEF_TITLE,
                    n.as_ptr(),
                    std::ffi::CString::new("string").unwrap().as_ptr(),
                    SIMCONNECT_DATATYPE_STRING256,
                    0.0,
                    0xFFFF_FFFF,
                );
                if hr < 0 {
                    logs.push(format!("SimConnect: AddToDef TITLE FAILED {}", hr_hex(hr)));
                }
            }

            {
                let n = std::ffi::CString::new("SIM ON GROUND").unwrap();
                let u = std::ffi::CString::new("Bool").unwrap();
                let hr = (fns.add_to_def)(
                    h_sc,
                    DEF_PING,
                    n.as_ptr(),
                    u.as_ptr(),
                    SIMCONNECT_DATATYPE_FLOAT64,
                    0.0,
                    0xFFFF_FFFF,
                );
                if hr < 0 {
                    logs.push(format!("SimConnect: AddToDef PING FAILED {}", hr_hex(hr)));
                }
            }

            let _ = (fns.req_data)(
                h_sc,
                REQ_TITLE,
                DEF_TITLE,
                USER_OBJECT_ID,
                SIMCONNECT_PERIOD_ONCE,
                0,
                0,
                0,
                0,
            );
            let _ = (fns.req_data)(
                h_sc,
                REQ_MAIN,
                DEF_MAIN,
                USER_OBJECT_ID,
                SIMCONNECT_PERIOD_SIM_FRAME,
                0,
                0,
                0,
                0,
            );
            thread::sleep(Duration::from_millis(60));
            let _ = (fns.req_data)(
                h_sc,
                REQ_MAIN,
                DEF_MAIN,
                USER_OBJECT_ID,
                SIMCONNECT_PERIOD_SIM_FRAME,
                0,
                0,
                0,
                0,
            );
            let _ = (fns.req_data)(
                h_sc,
                REQ_PING,
                DEF_PING,
                USER_OBJECT_ID,
                SIMCONNECT_PERIOD_ONCE,
                0,
                0,
                0,
                0,
            );

            let mut last_cfg_rev = config.current_rev();
            let mut bg_smoothed: f64 = 0.0;

            let mut main_seen = false;
            let mut last_main_rx = Instant::now();

            let mut prev_flaps_pct: f64 = 0.0;
            let mut prev_flaps_idx: i32 = 0;
            let mut flap_t0: f64 = -1.0;
            let mut flap_t1: f64 = -1.0;
            let mut flap_peak: f64 = 0.0;

            let mut prev_gear: f64 = 0.0;
            let mut gear_t0: f64 = -1.0;
            let mut gear_t1: f64 = -1.0;
            let mut gear_peak: f64 = 0.0;

            let mut paused_event_flag: bool = false;
            let mut paused_ex1_bits: u32 = 0;

            loop {
                let mut p_recv: *mut SimRecv = std::ptr::null_mut();
                let mut cb: DWord = 0;
                let hr = (fns.next_dispatch)(h_sc, &mut p_recv, &mut cb);

                if hr < 0 {
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }

                if !p_recv.is_null() && cb >= std::mem::size_of::<SimRecv>() as u32 {
                    match (*p_recv).dw_id {
                        SIMCONNECT_RECV_ID_OPEN => {}
                        SIMCONNECT_RECV_ID_QUIT => {
                            break;
                        }
                        SIMCONNECT_RECV_ID_EVENT => {
                            let ev = &*(p_recv as *const SimRecvEvent);

                            if ev.u_event_id == EVT_SIM_START {
                                in_flight = true;
                                *last_vars.lock() = None;
                                effects.flaps_bump_active.store(false, Ordering::Relaxed);
                                effects.gear_bump_active.store(false, Ordering::Relaxed);
                                effects.ground_active.store(false, Ordering::Relaxed);
                                effects.ground_thump_active.store(false, Ordering::Relaxed);
                                effects.base_active.store(false, Ordering::Relaxed);
                                effects.bank_active.store(false, Ordering::Relaxed);
                                effects.stall_active.store(false, Ordering::Relaxed);
                            } else if ev.u_event_id == EVT_SIM_STOP {
                                in_flight = false;
                                let _ = tx_hid.send(HidCmd::SendIntensity(0));
                                *last_vars.lock() = None;
                                effects.flaps_bump_active.store(false, Ordering::Relaxed);
                                effects.gear_bump_active.store(false, Ordering::Relaxed);
                                effects.ground_active.store(false, Ordering::Relaxed);
                                effects.ground_thump_active.store(false, Ordering::Relaxed);
                                effects.base_active.store(false, Ordering::Relaxed);
                                effects.bank_active.store(false, Ordering::Relaxed);
                                effects.stall_active.store(false, Ordering::Relaxed);
                            } else if ev.u_event_id == EVT_PAUSE_SYS {
                                paused_event_flag = ev.dw_data != 0;
                            } else if ev.u_event_id == EVT_PAUSE_EX1_SYS {
                                paused_ex1_bits = ev.dw_data;
                            }
                        }
                        SIMCONNECT_RECV_ID_SIMOBJECT_DATA => {
                            let sod = &*(p_recv as *const SimRecvSimObjectData);
                            let base_ptr = p_recv as *const u8;
                            let data_ptr = (&sod.dw_data as *const DWord) as *const u8;
                            let header_bytes =
                                (data_ptr as usize).saturating_sub(base_ptr as usize);
                            let payload_len = (cb as usize).saturating_sub(header_bytes);

                            if sod.dw_request_id == REQ_TITLE {
                                if payload_len >= 256 {
                                    let s_ptr = data_ptr as *const c_char;
                                    let title =
                                        CStr::from_ptr(s_ptr).to_string_lossy().into_owned();
                                    *aircraft_title.lock() = title;
                                }
                                continue;
                            }

                            if sod.dw_request_id == REQ_MAIN {
                                main_seen = true;
                                last_main_rx = Instant::now();

                                if !in_flight {
                                    *status.lock() = SimStatus::Connected;
                                    *last_vars.lock() = None;
                                    let _ = tx_hid.send(HidCmd::SendIntensity(0));
                                    effects.flaps_bump_active.store(false, Ordering::Relaxed);
                                    effects.gear_bump_active.store(false, Ordering::Relaxed);
                                    effects.ground_active.store(false, Ordering::Relaxed);
                                    effects.ground_thump_active.store(false, Ordering::Relaxed);
                                    effects.base_active.store(false, Ordering::Relaxed);
                                    effects.bank_active.store(false, Ordering::Relaxed);
                                    effects.stall_active.store(false, Ordering::Relaxed);
                                    continue;
                                }

                                let count = sod.dw_define_count as usize;
                                if count == 0 {
                                    continue;
                                }

                                let want_f64 = payload_len >= count * 8;
                                let want_f32 = !want_f64 && payload_len >= count * 4;
                                if !want_f64 && !want_f32 {
                                    continue;
                                }

                                let mut elem = [0f64; 11];
                                if want_f64 {
                                    let v = std::slice::from_raw_parts(
                                        data_ptr as *const f64,
                                        count.min(11),
                                    );
                                    for (i, &x) in v.iter().enumerate() {
                                        elem[i] = x;
                                    }
                                } else {
                                    let v = std::slice::from_raw_parts(
                                        data_ptr as *const f32,
                                        count.min(11),
                                    );
                                    for (i, &x) in v.iter().enumerate() {
                                        elem[i] = x as f64;
                                    }
                                }

                                let paused_from_var = elem.get(10).copied().unwrap_or(0.0) != 0.0;
                                let paused_from_events =
                                    paused_event_flag || (paused_ex1_bits != 0);

                                let mut fv = FlightVars {
                                    airspeed_indicated: elem.get(0).copied().unwrap_or(0.0),
                                    on_ground: elem.get(1).copied().unwrap_or(0.0) != 0.0,
                                    bank_deg: elem.get(2).copied().unwrap_or(0.0),
                                    flaps_pct: ((elem.get(3).copied().unwrap_or(0.0)
                                        + elem.get(4).copied().unwrap_or(0.0))
                                        * 0.5)
                                        .clamp(0.0, 100.0),
                                    flaps_index: elem.get(5).copied().unwrap_or(0.0).round() as i32,
                                    gear_handle: elem.get(6).copied().unwrap_or(0.0),
                                    stalled: elem.get(7).copied().unwrap_or(0.0) != 0.0,
                                    sim_time_s: elem.get(8).copied().unwrap_or(0.0),
                                    ground_speed_kt: elem.get(9).copied().unwrap_or(0.0).max(0.0),
                                    paused: paused_from_events || paused_from_var,
                                };

                                if !fv.airspeed_indicated.is_finite()
                                    || fv.airspeed_indicated < -5.0
                                    || fv.airspeed_indicated > 1200.0
                                {
                                    fv.airspeed_indicated = 0.0;
                                }
                                let cfg_now = config.get();
                                if fv.airspeed_indicated.abs() < cfg_now.ias_deadband_kn {
                                    fv.airspeed_indicated = 0.0;
                                }
                                if !fv.bank_deg.is_finite() {
                                    fv.bank_deg = 0.0;
                                }

                                *last_vars.lock() = Some(fv);

                                *status.lock() = if !fv.on_ground && fv.airspeed_indicated > 30.0 {
                                    SimStatus::InFlight
                                } else {
                                    SimStatus::Connected
                                };

                                let gs = fv.ground_speed_kt;
                                let start = cfg_now.taxi_start_kn.min(cfg_now.taxi_end_kn - 0.1);
                                let end = cfg_now.taxi_end_kn.max(start + 0.1);

                                let in_thump_band = fv.on_ground && gs >= start && gs < end;
                                let at_or_above_end = fv.on_ground && gs >= end;
                                let at_or_above_start = fv.on_ground && gs >= start;

                                effects
                                    .taxi_start_crossed
                                    .store(at_or_above_start, Ordering::Relaxed);
                                effects
                                    .taxi_end_crossed
                                    .store(at_or_above_end, Ordering::Relaxed);
                                effects
                                    .ground_thump_active
                                    .store(in_thump_band, Ordering::Relaxed);
                                effects
                                    .ground_active
                                    .store(at_or_above_end, Ordering::Relaxed);

                                effects.stall_active.store(fv.stalled, Ordering::Relaxed);
                                effects.bank_active.store(
                                    !fv.on_ground && fv.bank_deg.abs() > 5.0,
                                    Ordering::Relaxed,
                                );
                                effects.base_active.store(
                                    !fv.on_ground && fv.airspeed_indicated > 30.0,
                                    Ordering::Relaxed,
                                );

                                if fv.paused || hold.load(Ordering::Relaxed) {
                                    let _ = tx_hid.send(HidCmd::SendIntensity(0));
                                    continue;
                                }

                                if fv.flaps_index != prev_flaps_idx {
                                    let steps =
                                        (fv.flaps_index - prev_flaps_idx).abs().max(1) as usize;
                                    flap_t0 = fv.sim_time_s;
                                    flap_t1 = fv.sim_time_s
                                        + cfg_now.flaps_bump_duration_s * steps as f64;
                                    flap_peak = cfg_now.flaps_peak as f64;
                                    prev_flaps_idx = fv.flaps_index;
                                } else {
                                    let dflap = (fv.flaps_pct - prev_flaps_pct).abs();
                                    if dflap >= cfg_now.flaps_bump_eps_pct {
                                        flap_t0 = fv.sim_time_s;
                                        flap_t1 = fv.sim_time_s + cfg_now.flaps_bump_duration_s;
                                        let scale = (dflap / 12.5).clamp(0.5, 1.0);
                                        flap_peak = (cfg_now.flaps_peak as f64) * scale;
                                    }
                                    prev_flaps_pct = fv.flaps_pct;
                                }

                                if (fv.gear_handle - prev_gear).abs() >= 0.5 {
                                    gear_t0 = fv.sim_time_s;
                                    gear_t1 = fv.sim_time_s + cfg_now.gear_bump_duration_s;
                                    gear_peak = cfg_now.gear_peak as f64;
                                }
                                prev_gear = fv.gear_handle;

                                let mut ground_term = 0.0;

                                if fv.on_ground && gs >= start {
                                    let t_norm = ((gs - start) / (end - start)).clamp(0.0, 1.0);

                                    let period = cfg_now.thump_max_period_s
                                        - t_norm
                                            * (cfg_now.thump_max_period_s
                                                - cfg_now.thump_min_period_s);

                                    let cycle = (fv.sim_time_s / period).fract();

                                    let duty = cfg_now.thump_duty.clamp(0.05, 0.4);
                                    let in_pulse = cycle < duty;
                                    let thump_env = if in_pulse {
                                        let p = (cycle / duty).clamp(0.0, 1.0);
                                        (std::f64::consts::PI * p).sin()
                                    } else {
                                        0.0
                                    };

                                    let amp = (cfg_now.ground_roll as f64) * (0.35 + 0.65 * t_norm);

                                    ground_term = thump_env * amp;

                                    if gs >= end {
                                        let f_hz = 8.0;
                                        let phase =
                                            (2.0 * std::f64::consts::PI * f_hz * fv.sim_time_s)
                                                .sin()
                                                * 0.5
                                                + 0.5;
                                        ground_term = (cfg_now.ground_roll as f64) * phase;
                                    }
                                }

                                let mut air_term = 0.0;
                                if !fv.on_ground && fv.airspeed_indicated > 30.0 {
                                    air_term += (fv.airspeed_indicated / 250.0).clamp(0.0, 1.0)
                                        * (cfg_now.base_airspeed as f64);
                                }
                                if !fv.on_ground {
                                    let bank = fv.bank_deg.abs().min(45.0) / 45.0;
                                    air_term += bank * (cfg_now.bank as f64);
                                }

                                let bg = air_term + ground_term;
                                if config.current_rev() != last_cfg_rev {
                                    bg_smoothed = bg;
                                    last_cfg_rev = config.current_rev();
                                } else {
                                    let alpha = cfg_now.smoothing_alpha.clamp(0.0, 1.0) as f64;
                                    bg_smoothed = bg_smoothed + alpha * (bg - bg_smoothed);
                                }

                                let mut transients: f64 = 0.0;
                                if fv.stalled {
                                    transients = transients.max(cfg_now.stall_ceiling as f64);
                                }
                                let flap_active = fv.sim_time_s >= flap_t0
                                    && fv.sim_time_s <= flap_t1
                                    && flap_peak > 0.0;
                                let gear_active = fv.sim_time_s >= gear_t0
                                    && fv.sim_time_s <= gear_t1
                                    && gear_peak > 0.0;
                                if flap_active {
                                    let elapsed = fv.sim_time_s - flap_t0;
                                    let period = 1.0_f64.max(cfg_now.flaps_bump_duration_s);
                                    let phase = (elapsed % period) / period;
                                    transients += flap_peak * (std::f64::consts::PI * phase).sin();
                                }
                                if gear_active {
                                    let p = ((fv.sim_time_s - gear_t0) / (gear_t1 - gear_t0))
                                        .clamp(0.0, 1.0);
                                    transients += gear_peak * (std::f64::consts::PI * p).sin();
                                }
                                effects
                                    .flaps_bump_active
                                    .store(flap_active, Ordering::Relaxed);
                                effects
                                    .gear_bump_active
                                    .store(gear_active, Ordering::Relaxed);

                                let mut total = bg_smoothed + transients;
                                if fv.stalled {
                                    total = total.max(cfg_now.stall_ceiling as f64);
                                }
                                total = total.clamp(0.0, cfg_now.max_output as f64);
                                let _ = tx_hid.send(HidCmd::SendIntensity(total.round() as u8));
                            }
                        }
                        SIMCONNECT_RECV_ID_EXCEPTION => {}
                        _ => {}
                    }
                } else {
                    thread::sleep(Duration::from_millis(10));
                }

                let timeout = if main_seen {
                    Duration::from_millis(2500)
                } else {
                    Duration::from_millis(800)
                };
                if last_main_rx.elapsed() >= timeout {
                    let _ = (fns.req_data)(
                        h_sc,
                        REQ_MAIN,
                        DEF_MAIN,
                        USER_OBJECT_ID,
                        SIMCONNECT_PERIOD_SIM_FRAME,
                        0,
                        0,
                        0,
                        0,
                    );
                    last_main_rx = Instant::now();
                }
            }

            let _ = (fns.close)(h_sc);
            *status.lock() = SimStatus::Disconnected;
            *aircraft_title.lock() = String::new();
            *last_vars.lock() = None;
            let _ = tx_hid.send(HidCmd::SendIntensity(0));
            thread::sleep(Duration::from_millis(600));
        }
    }
}
