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

use crate::preset::{
    is_engine_extra_key, PresetShared, SimVarLayout, SimVarProfile,
    CORE_SIMVARS, CORE_SIMVAR_COUNT,
};
use crate::rumble::RumbleEngine;
use crate::sim::parse::{
    finalize_flight_vars, flight_status, merge_extras, parse_extra_elems, parse_main_elems,
};
use crate::{EffectsShared, FlightVars, HidCmd, LogBuffer, SimStatus};

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
    dw_data: DWord,
}

#[repr(C)]
struct SimRecvEvent {
    base: SimRecv,
    u_group_id: DWord,
    u_event_id: DWord,
    dw_data: DWord,
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

const SIMCONNECT_UNUSED: DWord = 0xFFFF_FFFF;

const USER_OBJECT_ID: DWord = 0;

const EVT_SIM_START: DWord = 1001;
const EVT_SIM_STOP: DWord = 1002;
const EVT_FRAME: DWord = 1003;

const DEF_CORE: DWord = 2001;
const REQ_CORE: DWord = 3001;
const DEF_ENGINE: DWord = 2003;
const REQ_ENGINE: DWord = 3003;
const DEF_EXTRAS: DWord = 2002;
const REQ_EXTRAS: DWord = 3002;
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

pub fn sim_worker(
    last_vars: Arc<Mutex<Option<FlightVars>>>,
    tx_hid: Sender<HidCmd>,
    logs: LogBuffer,
    preset: Arc<PresetShared>,
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

            let mut rumble_engine = RumbleEngine::new();
            let session_simvars = preset.simvar_profile();
            let core_field_count = CORE_SIMVAR_COUNT;

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

                logs.push("SimConnect: event subscriptions active.".to_string());
            }

            let add = |def_id: DWord, name_s: &str, unit_s: &str, datum_id: DWord| -> HRESULT {
                let n = std::ffi::CString::new(name_s).unwrap();
                let u = std::ffi::CString::new(unit_s).unwrap();
                (fns.add_to_def)(
                    h_sc,
                    def_id,
                    n.as_ptr(),
                    u.as_ptr(),
                    SIMCONNECT_DATATYPE_FLOAT64,
                    0.0,
                    datum_id,
                )
            };

            let core_fields = SimVarLayout::core_only().fields;
            let mut session_core_layout = SimVarLayout { fields: Vec::new() };
            let mut session_engine_keys: Vec<String> = Vec::new();
            let mut session_extra_keys: Vec<String> = Vec::new();
            let mut core_registered = 0usize;
            let mut engine_registered = 0usize;
            let mut extras_registered = 0usize;

            for (i, (name, unit)) in CORE_SIMVARS.iter().enumerate() {
                let reg_name = SimVarProfile::simconnect_datum_name(name, SIMCONNECT_UNUSED);
                let hr = add(DEF_CORE, &reg_name, unit, SIMCONNECT_UNUSED);
                if hr < 0 {
                    logs.push(format!(
                        "SimConnect: core {:?} → {:?} FAILED {}",
                        name,
                        reg_name,
                        hr_hex(hr)
                    ));
                    continue;
                }
                core_registered += 1;
                session_core_layout.fields.push(core_fields[i].clone());
            }

            for def in &session_simvars.extra {
                let reg_name = SimVarProfile::simconnect_datum_name(&def.name, def.datum_index);
                let def_id = if is_engine_extra_key(&def.key) {
                    DEF_ENGINE
                } else {
                    DEF_EXTRAS
                };
                let hr = add(def_id, &reg_name, &def.unit, SIMCONNECT_UNUSED);
                if hr < 0 {
                    logs.push(format!(
                        "SimConnect: extra {} {:?} → {:?} FAILED {}",
                        def.key,
                        def.name,
                        reg_name,
                        hr_hex(hr)
                    ));
                    continue;
                }
                if is_engine_extra_key(&def.key) {
                    engine_registered += 1;
                    session_engine_keys.push(def.key.clone());
                } else {
                    extras_registered += 1;
                    session_extra_keys.push(def.key.clone());
                }
                logs.push(format!(
                    "SimConnect: extra {} → {} [{}]",
                    def.key, reg_name, def.unit
                ));
            }

            logs.push(format!(
                "SimConnect: DEF_CORE {core_registered}/{core_field_count}, DEF_ENGINE {engine_registered}/{}, DEF_EXTRAS {extras_registered}/{}",
                session_engine_keys.len(),
                session_extra_keys.len()
            ));

            {
                let n = std::ffi::CString::new("TITLE").unwrap();
                let hr = (fns.add_to_def)(
                    h_sc,
                    DEF_TITLE,
                    n.as_ptr(),
                    std::ffi::CString::new("string").unwrap().as_ptr(),
                    SIMCONNECT_DATATYPE_STRING256,
                    0.0,
                    SIMCONNECT_UNUSED,
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
                    SIMCONNECT_UNUSED,
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
                REQ_CORE,
                DEF_CORE,
                USER_OBJECT_ID,
                SIMCONNECT_PERIOD_SIM_FRAME,
                0,
                0,
                0,
                0,
            );
            if !session_engine_keys.is_empty() {
                let _ = (fns.req_data)(
                    h_sc,
                    REQ_ENGINE,
                    DEF_ENGINE,
                    USER_OBJECT_ID,
                    SIMCONNECT_PERIOD_SIM_FRAME,
                    0,
                    0,
                    0,
                    0,
                );
            }
            if !session_extra_keys.is_empty() {
                let _ = (fns.req_data)(
                    h_sc,
                    REQ_EXTRAS,
                    DEF_EXTRAS,
                    USER_OBJECT_ID,
                    SIMCONNECT_PERIOD_SIM_FRAME,
                    0,
                    0,
                    0,
                    0,
                );
            }
            thread::sleep(Duration::from_millis(60));
            let _ = (fns.req_data)(
                h_sc,
                REQ_CORE,
                DEF_CORE,
                USER_OBJECT_ID,
                SIMCONNECT_PERIOD_SIM_FRAME,
                0,
                0,
                0,
                0,
            );
            if !session_engine_keys.is_empty() {
                let _ = (fns.req_data)(
                    h_sc,
                    REQ_ENGINE,
                    DEF_ENGINE,
                    USER_OBJECT_ID,
                    SIMCONNECT_PERIOD_SIM_FRAME,
                    0,
                    0,
                    0,
                    0,
                );
            }
            if !session_extra_keys.is_empty() {
                let _ = (fns.req_data)(
                    h_sc,
                    REQ_EXTRAS,
                    DEF_EXTRAS,
                    USER_OBJECT_ID,
                    SIMCONNECT_PERIOD_SIM_FRAME,
                    0,
                    0,
                    0,
                    0,
                );
            }
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

            let mut latest_engine_extras = std::collections::HashMap::new();
            let mut latest_other_extras = std::collections::HashMap::new();
            let mut core_fv_base: Option<FlightVars> = None;
            let mut main_seen = false;
            let mut last_main_rx = Instant::now();

            let mut simvar_mismatch_logged = std::collections::HashSet::new();
            let mut last_rumble_log = Instant::now();
            let mut last_logged_intensity: u8 = 255;
            let mut main_frame_count: u64 = 0;
            let mut last_frame_diag = Instant::now();

            loop {
                if preset.simvar_profile() != session_simvars {
                    logs.push("SimConnect: preset simvars changed, reconnecting".to_string());
                    break;
                }

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
                                *last_vars.lock() = None;
                                rumble_engine.reset();
                                effects.clear_all();
                            } else if ev.u_event_id == EVT_SIM_STOP {
                                let _ = tx_hid.send(HidCmd::SendIntensity(0));
                                *last_vars.lock() = None;
                                effects.clear_all();
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

                            if sod.dw_request_id == REQ_CORE
                                || sod.dw_request_id == REQ_EXTRAS
                                || sod.dw_request_id == REQ_ENGINE
                            {
                                main_seen = true;
                                last_main_rx = Instant::now();
                                if sod.dw_request_id == REQ_CORE {
                                    main_frame_count += 1;
                                }

                                let count = sod.dw_define_count as usize;
                                if count == 0 {
                                    continue;
                                }

                                let (packet_name, expected, max_elem_count, keys) =
                                    if sod.dw_request_id == REQ_CORE {
                                        (
                                            "DEF_CORE",
                                            session_core_layout.total_count(),
                                            core_field_count,
                                            None,
                                        )
                                    } else if sod.dw_request_id == REQ_ENGINE {
                                        (
                                            "DEF_ENGINE",
                                            session_engine_keys.len(),
                                            session_engine_keys.len(),
                                            Some(&session_engine_keys),
                                        )
                                    } else {
                                        (
                                            "DEF_EXTRAS",
                                            session_extra_keys.len(),
                                            session_extra_keys.len(),
                                            Some(&session_extra_keys),
                                        )
                                    };

                                if count != expected
                                    && !simvar_mismatch_logged.contains(packet_name)
                                {
                                    logs.push(format!(
                                        "SimConnect: {packet_name} count mismatch (got {count}, expected {expected}) — parsing first {count} fields"
                                    ));
                                    simvar_mismatch_logged.insert(packet_name);
                                }

                                let want_f64 = payload_len >= count * 8;
                                let want_f32 = !want_f64 && payload_len >= count * 4;
                                if !want_f64 && !want_f32 {
                                    continue;
                                }

                                let mut elem = vec![0f64; max_elem_count.max(count)];
                                if want_f64 {
                                    let v = std::slice::from_raw_parts(
                                        data_ptr as *const f64,
                                        count.min(elem.len()),
                                    );
                                    for (i, &x) in v.iter().enumerate() {
                                        elem[i] = x;
                                    }
                                } else {
                                    let v = std::slice::from_raw_parts(
                                        data_ptr as *const f32,
                                        count.min(elem.len()),
                                    );
                                    for (i, &x) in v.iter().enumerate() {
                                        elem[i] = x as f64;
                                    }
                                }

                                if sod.dw_request_id == REQ_ENGINE {
                                    let keys = keys.expect("engine keys");
                                    let field_count = count.min(keys.len());
                                    latest_engine_extras = parse_extra_elems(
                                        &elem[..field_count],
                                        &keys[..field_count],
                                    );
                                    if let Some(mut fv) = core_fv_base.clone() {
                                        merge_extras(&mut fv, &latest_engine_extras);
                                        merge_extras(&mut fv, &latest_other_extras);
                                        finalize_flight_vars(&mut fv);
                                        let cfg_now = preset.rumble_config();
                                        *status.lock() = flight_status(&fv);
                                        let out = rumble_engine.step(
                                            &fv,
                                            &cfg_now,
                                            preset.current_rev(),
                                            hold.load(Ordering::Relaxed),
                                        );
                                        effects.apply_snapshot(&out.effects);
                                        *last_vars.lock() = Some(fv);
                                        let _ = tx_hid.send(HidCmd::SendIntensity(out.intensity));
                                    }
                                    continue;
                                }

                                if sod.dw_request_id == REQ_EXTRAS {
                                    let keys = keys.expect("extras keys");
                                    let field_count = count.min(keys.len());
                                    latest_other_extras = parse_extra_elems(
                                        &elem[..field_count],
                                        &keys[..field_count],
                                    );
                                    if let Some(mut fv) = core_fv_base.clone() {
                                        merge_extras(&mut fv, &latest_engine_extras);
                                        merge_extras(&mut fv, &latest_other_extras);
                                        finalize_flight_vars(&mut fv);
                                        let cfg_now = preset.rumble_config();
                                        *status.lock() = flight_status(&fv);
                                        let out = rumble_engine.step(
                                            &fv,
                                            &cfg_now,
                                            preset.current_rev(),
                                            hold.load(Ordering::Relaxed),
                                        );
                                        effects.apply_snapshot(&out.effects);
                                        *last_vars.lock() = Some(fv);
                                        let _ = tx_hid.send(HidCmd::SendIntensity(out.intensity));
                                    }
                                    continue;
                                }

                                let field_count = count.min(session_core_layout.fields.len());
                                let parse_layout = SimVarLayout {
                                    fields: session_core_layout.fields[..field_count].to_vec(),
                                };

                                let cfg_now = preset.rumble_config();
                                let mut fv = parse_main_elems(
                                    &elem[..field_count],
                                    &parse_layout,
                                    false,
                                    cfg_now.ias_deadband_kn,
                                );
                                core_fv_base = Some(fv.clone());
                                merge_extras(&mut fv, &latest_engine_extras);
                                merge_extras(&mut fv, &latest_other_extras);
                                finalize_flight_vars(&mut fv);

                                *status.lock() = flight_status(&fv);

                                let out = rumble_engine.step(
                                    &fv,
                                    &cfg_now,
                                    preset.current_rev(),
                                    hold.load(Ordering::Relaxed),
                                );
                                effects.apply_snapshot(&out.effects);
                                if out.intensity != last_logged_intensity
                                    || (out.intensity > 0
                                        && last_rumble_log.elapsed() > Duration::from_secs(5))
                                {
                                    logs.push(format!(
                                        "Sim: rumble intensity {} (eng_rpm={:.0}, paused={}, gs={:.1}, ias={:.1}, on_ground={}, engine_dot={})",
                                        out.intensity,
                                        fv.eng_rpm,
                                        fv.paused,
                                        fv.ground_speed_kt,
                                        fv.airspeed_indicated,
                                        fv.on_ground,
                                        out.effects.engine_vibe_active,
                                    ));
                                    last_logged_intensity = out.intensity;
                                    last_rumble_log = Instant::now();
                                }
                                *last_vars.lock() = Some(fv);
                                let _ = tx_hid.send(HidCmd::SendIntensity(out.intensity));
                            }
                        }
                        SIMCONNECT_RECV_ID_EXCEPTION => {}
                        _ => {}
                    }
                } else {
                    thread::sleep(Duration::from_millis(10));
                }

                if last_frame_diag.elapsed() >= Duration::from_secs(5) {
                    if main_frame_count > 0 {
                        if let Some(fv) = last_vars.lock().as_ref() {
                            logs.push(format!(
                                "Sim: {main_frame_count} frames/5s — ias={:.0} gs={:.1} eng_rpm={:.0} on_ground={} paused={} hold={}",
                                fv.airspeed_indicated,
                                fv.ground_speed_kt,
                                fv.eng_rpm,
                                fv.on_ground,
                                fv.paused,
                                hold.load(Ordering::Relaxed),
                            ));
                        } else {
                            logs.push(format!(
                                "Sim: {main_frame_count} frames/5s — no flight vars yet"
                            ));
                        }
                    } else if main_seen {
                        logs.push(
                            "Sim: no main frames in last 5s (sim paused or disconnected?)"
                                .to_string(),
                        );
                    }
                    main_frame_count = 0;
                    last_frame_diag = Instant::now();
                }

                let timeout = if main_seen {
                    Duration::from_millis(2500)
                } else {
                    Duration::from_millis(800)
                };
                if last_main_rx.elapsed() >= timeout {
                    let _ = (fns.req_data)(
                        h_sc,
                        REQ_CORE,
                        DEF_CORE,
                        USER_OBJECT_ID,
                        SIMCONNECT_PERIOD_SIM_FRAME,
                        0,
                        0,
                        0,
                        0,
                    );
                    if !session_engine_keys.is_empty() {
                        let _ = (fns.req_data)(
                            h_sc,
                            REQ_ENGINE,
                            DEF_ENGINE,
                            USER_OBJECT_ID,
                            SIMCONNECT_PERIOD_SIM_FRAME,
                            0,
                            0,
                            0,
                            0,
                        );
                    }
                    if !session_extra_keys.is_empty() {
                        let _ = (fns.req_data)(
                            h_sc,
                            REQ_EXTRAS,
                            DEF_EXTRAS,
                            USER_OBJECT_ID,
                            SIMCONNECT_PERIOD_SIM_FRAME,
                            0,
                            0,
                            0,
                            0,
                        );
                    }
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
