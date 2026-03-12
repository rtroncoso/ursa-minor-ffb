use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use hidapi::{HidApi, HidDevice};

use windows::core::PCWSTR;
use windows::Win32::Devices::HumanInterfaceDevice::{
    HidD_FreePreparsedData, HidD_GetPreparsedData, HidP_GetCaps, HidP_GetValueCaps, HidP_Output,
    HIDP_CAPS, HIDP_STATUS_SUCCESS, HIDP_VALUE_CAPS,
};
use windows::Win32::Foundation::{HANDLE, NTSTATUS};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_CREATION_DISPOSITION, FILE_FLAGS_AND_ATTRIBUTES,
    FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_MODE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING,
};

use crate::{log, HidCmd};
use log::LogBuffer;

// -----------------------------
// Winwing IDs
// -----------------------------
const WW_VID: u16 = 0x4098;

const WW_PID_URSA_MINOR_AIRBUS_L: u16 = 0xBC27;
const WW_PID_URSA_MINOR_AIRBUS_R: u16 = 0xBC28;
const WW_PID_URSA_MINOR_FIGHTER_L: u16 = 0xBC29;
const WW_PID_URSA_MINOR_FIGHTER_R: u16 = 0xBC2A;
const WW_PID_URSA_MINOR_SPACE_L: u16 = 0xBC2B;
const WW_PID_URSA_MINOR_SPACE_R: u16 = 0xBC2C;

// -----------------------------
// HID worker
// -----------------------------
struct HidEntry {
    dev: HidDevice,
    path: String,
    pid: u16,
    ifnum: i32,
    usage_page: u16,
    usage: u16,
    out_len: u16,
    report_id: u8,
}

// -----------------------------
// Helpers
// -----------------------------

fn ursa_model_name(pid: u16) -> &'static str {
    match pid {
        WW_PID_URSA_MINOR_AIRBUS_L => "URSA MINOR AIRBUS L",
        WW_PID_URSA_MINOR_AIRBUS_R => "URSA MINOR AIRBUS R",
        WW_PID_URSA_MINOR_FIGHTER_L => "URSA MINOR FIGHTER L",
        WW_PID_URSA_MINOR_FIGHTER_R => "URSA MINOR FIGHTER R",
        WW_PID_URSA_MINOR_SPACE_L => "URSA MINOR SPACE L",
        WW_PID_URSA_MINOR_SPACE_R => "URSA MINOR SPACE R",
        _ => "UNKNOWN",
    }
}

fn is_ursa_minor_left(pid: u16) -> bool {
    matches!(
        pid,
        WW_PID_URSA_MINOR_AIRBUS_L | WW_PID_URSA_MINOR_FIGHTER_L | WW_PID_URSA_MINOR_SPACE_L
    )
}

fn is_ursa_minor_right(pid: u16) -> bool {
    matches!(
        pid,
        WW_PID_URSA_MINOR_AIRBUS_R | WW_PID_URSA_MINOR_FIGHTER_R | WW_PID_URSA_MINOR_SPACE_R
    )
}

fn handed_selector_for_pid(pid: u16) -> u8 {
    if is_ursa_minor_right(pid) {
        0x08
    } else if is_ursa_minor_left(pid) {
        0x07
    } else {
        0x07
    }
}

fn build_simapp_vibe_frame(pid: u16, report_id: u8, out_len: u16, intensity: u8) -> Vec<u8> {
    // Body without report ID (13 bytes):
    //
    // Right-handed URSA MINOR variants:
    // 08 BF 00 00 03 49 00 <intensity> 00 00 00 00 00
    //
    // Left-handed URSA MINOR variants:
    // 07 BF 00 00 03 49 00 <intensity> 00 00 00 00 00
    //
    // Known mappings:
    //   0xBC27 => Airbus L
    //   0xBC28 => Airbus R
    //   0xBC29 => Fighter L
    //   0xBC2A => Fighter R
    //   0xBC2B => Space L
    //   0xBC2C => Space R
    let handed_selector = handed_selector_for_pid(pid);

    let body: [u8; 13] = [
        handed_selector,
        0xBF,
        0x00,
        0x00,
        0x03,
        0x49,
        0x00,
        intensity,
        0,
        0,
        0,
        0,
        0,
    ];

    let len = out_len as usize;
    let mut buf = vec![0u8; len];

    if len == 0 {
        return buf;
    }

    buf[0] = report_id;
    let copy_len = body.len().min(len.saturating_sub(1));
    buf[1..1 + copy_len].copy_from_slice(&body[..copy_len]);
    buf
}

/// Query Output report size and report ID using HID parser caps.
/// Returns (out_len, report_id). Falls back to (14, 0x02) if anything fails.
/// Logs all discovered OUTPUT ReportIDs for the device path.
fn hid_query_caps_from_path(path_utf8: &str, logs: &LogBuffer) -> Option<(u16, u8)> {
    let wide: Vec<u16> = OsStr::new(path_utf8)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let desired: u32 = (FILE_GENERIC_READ | FILE_GENERIC_WRITE).0;
    let share: FILE_SHARE_MODE = FILE_SHARE_READ | FILE_SHARE_WRITE;
    let disp: FILE_CREATION_DISPOSITION = OPEN_EXISTING;
    let attrs: FILE_FLAGS_AND_ATTRIBUTES = FILE_ATTRIBUTE_NORMAL;

    let h: HANDLE = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            desired,
            share,
            None,
            disp,
            attrs,
            None,
        )
        .ok()?
    };

    let mut pp = windows::Win32::Devices::HumanInterfaceDevice::PHIDP_PREPARSED_DATA::default();
    let got_pp = unsafe { HidD_GetPreparsedData(h, &mut pp as *mut _) }.as_bool();
    if !got_pp {
        let _ = unsafe { windows::Win32::Foundation::CloseHandle(h) };
        logs.push(format!(
            "HID: caps path='{}' → GetPreparsedData FAILED",
            path_utf8
        ));
        return None;
    }

    let mut caps = HIDP_CAPS::default();
    let st_caps: NTSTATUS = unsafe { HidP_GetCaps(pp, &mut caps) };
    if st_caps != HIDP_STATUS_SUCCESS {
        let _ = unsafe { HidD_FreePreparsedData(pp) };
        let _ = unsafe { windows::Win32::Foundation::CloseHandle(h) };
        logs.push(format!(
            "HID: caps path='{}' → HidP_GetCaps FAILED",
            path_utf8
        ));
        return None;
    }

    let out_len: u16 = caps.OutputReportByteLength;

    let mut count: u16 = caps.NumberOutputValueCaps as u16;
    let mut value_caps: Vec<HIDP_VALUE_CAPS> = vec![HIDP_VALUE_CAPS::default(); count as usize];

    let st_vals: NTSTATUS = unsafe {
        HidP_GetValueCaps(
            HidP_Output,
            value_caps.as_mut_ptr(),
            &mut count as *mut u16,
            pp,
        )
    };

    let mut report_id: u8 = 0;
    let mut ids: Vec<u8> = Vec::new();

    if st_vals == HIDP_STATUS_SUCCESS && count > 0 {
        value_caps.truncate(count as usize);

        ids.extend(
            value_caps
                .iter()
                .map(|vc| vc.ReportID as u8)
                .filter(|&id| id != 0),
        );
        ids.sort_unstable();
        ids.dedup();

        if let Some(&min_nonzero) = ids.first() {
            report_id = min_nonzero;
        }
    }

    let _ = unsafe { HidD_FreePreparsedData(pp) };
    let _ = unsafe { windows::Win32::Foundation::CloseHandle(h) };

    let ids_fmt = if ids.is_empty() {
        "[]".to_string()
    } else {
        format!(
            "[{}]",
            ids.iter()
                .map(|id| format!("0x{:02X}", id))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };

    logs.push(format!(
        "HID: caps path='{}' → out_len={} output_report_ids={} chosen=0x{:02X}",
        path_utf8, out_len, ids_fmt, report_id
    ));

    Some((out_len, report_id))
}

fn hid_send_out(devs: &Vec<HidEntry>, intensity: u8, _logs: &LogBuffer) -> (usize, usize) {
    let mut ok = 0usize;
    let mut fail = 0usize;

    for d in devs {
        // Only act on joystick interfaces
        if !(d.usage_page == 0x0001 && d.usage == 0x0004) {
            continue;
        }

        let frame = build_simapp_vibe_frame(d.pid, d.report_id, d.out_len, intensity);
        match d.dev.write(&frame) {
            Ok(n) => {
                if n == frame.len() {
                    ok += 1;
                } else {
                    fail += 1;
                }
            }
            Err(_) => {
                fail += 1;
            }
        }
    }

    (ok, fail)
}

pub fn hid_worker(controller_connected: Arc<AtomicBool>, rx: Receiver<HidCmd>, logs: LogBuffer) {
    logs.push("HID: worker starting…");

    let verbose_hid = std::env::var_os("URSA_VERBOSE_HID").is_some();
    let mut api = match HidApi::new() {
        Ok(a) => {
            logs.push("HID: HidApi initialized");
            a
        }
        Err(e) => {
            logs.push(format!("HID: HidApi::new FAILED: {}", e));
            return;
        }
    };

    let mut devices: Vec<HidEntry> = vec![];
    let mut last_scan = Instant::now() - Duration::from_secs(10);
    let mut last_status_log = Instant::now() - Duration::from_secs(10);
    let mut last_missing_log = Instant::now() - Duration::from_secs(10);

    const SEND_INTERVAL: Duration = Duration::from_millis(50);

    let mut desired_intensity: u8 = 0;
    let mut last_sent_intensity: u8 = 255;
    let mut last_send: Instant = Instant::now() - SEND_INTERVAL;
    let mut hold: bool = false;
    let mut prev_scan_sig = String::new();

    let mut ensure_open = |api: &mut HidApi, devices: &mut Vec<HidEntry>| {
        if last_scan.elapsed() < Duration::from_secs(2) && !devices.is_empty() {
            return;
        }

        let mut idx_by_path: HashMap<String, usize> = HashMap::new();
        for (i, d) in devices.iter().enumerate() {
            idx_by_path.insert(d.path.clone(), i);
        }

        if let Err(e) = api.refresh_devices() {
            logs.push(format!("HID: refresh_devices FAILED: {}", e));
        }

        let mut seen_paths: HashSet<String> = HashSet::new();
        let mut found_summary: Vec<String> = Vec::new();

        for devinfo in api.device_list() {
            if devinfo.vendor_id() != WW_VID {
                continue;
            }

            let path = devinfo.path().to_string_lossy().to_string();
            let pid = devinfo.product_id();
            let ifnum = devinfo.interface_number();
            let up = devinfo.usage_page();
            let u = devinfo.usage();

            seen_paths.insert(path.clone());
            found_summary.push(format!(
                "pid=0x{:04X} ({}) if#{} up=0x{:04X} u=0x{:04X} path='{}'",
                pid,
                ursa_model_name(pid),
                ifnum,
                up,
                u,
                path
            ));
        }

        found_summary.sort();
        let scan_sig = found_summary.join(" | ");
        if scan_sig != prev_scan_sig {
            logs.push(if found_summary.is_empty() {
                "HID: scan found 0 Winwing devices".to_string()
            } else {
                format!(
                    "HID: scan found {} Winwing devices: {}",
                    found_summary.len(),
                    scan_sig
                )
            });
            prev_scan_sig = scan_sig;
        }

        for devinfo in api.device_list() {
            if devinfo.vendor_id() != WW_VID {
                continue;
            }

            let path = devinfo.path().to_string_lossy().to_string();
            if idx_by_path.contains_key(&path) {
                continue;
            }

            let pid = devinfo.product_id();
            let (out_len, report_id) =
                hid_query_caps_from_path(&path, &logs).unwrap_or((14u16, 0x02u8));

            let d = match devinfo.open_device(api) {
                Ok(d) => d,
                Err(e) => {
                    logs.push(format!("HID: open failed on '{}' : {}", path, e));
                    continue;
                }
            };

            devices.push(HidEntry {
                dev: d,
                path: path.clone(),
                pid,
                ifnum: devinfo.interface_number(),
                usage_page: devinfo.usage_page(),
                usage: devinfo.usage(),
                out_len,
                report_id,
            });

            logs.push(format!(
                "HID: device OPENED (VID=0x{:04X}, PID=0x{:04X} {}, if#{}, up=0x{:04X}, u=0x{:04X}, out_len={}, report_id=0x{:02X}) path='{}'",
                devinfo.vendor_id(),
                pid,
                format!("({})", ursa_model_name(pid)),
                devinfo.interface_number(),
                devinfo.usage_page(),
                devinfo.usage(),
                out_len,
                report_id,
                devinfo.path().to_string_lossy()
            ));
        }

        if !devices.is_empty() {
            devices.retain(|d| {
                if seen_paths.contains(&d.path) {
                    true
                } else {
                    logs.push(format!("HID: device REMOVED path='{}'", d.path));
                    false
                }
            });
        }

        if seen_paths.is_empty() && last_missing_log.elapsed() > Duration::from_secs(5) {
            logs.push("HID: no Winwing devices found (VID=0x4098)".to_string());
            last_missing_log = Instant::now();
        }

        controller_connected.store(!devices.is_empty(), Ordering::Relaxed);
        last_scan = Instant::now();
    };

    ensure_open(&mut api, &mut devices);

    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(cmd) => match cmd {
                HidCmd::SendIntensity(level) => {
                    desired_intensity = level;
                    if verbose_hid
                        && (i16::from(desired_intensity) - i16::from(last_sent_intensity)).abs()
                            >= 15
                    {
                        logs.push(format!("HID: cmd SendIntensity({})", desired_intensity));
                    }
                }
                HidCmd::SendRaw(bytes) => {
                    logs.push(format!("HID: cmd SendRaw(len={})", bytes.len()));
                    for d in &devices {
                        if let Err(e) = d.dev.write(&bytes) {
                            logs.push(format!(
                                "HID: raw write FAILED (PID=0x{:04X} {}, if#{}, usage_page=0x{:04X}, usage=0x{:04X}) path='{}': {}",
                                d.pid,
                                ursa_model_name(d.pid),
                                d.ifnum,
                                d.usage_page,
                                d.usage,
                                d.path,
                                e
                            ));
                        }
                    }
                }
                HidCmd::StopAll => {
                    logs.push("HID: cmd StopAll");
                    desired_intensity = 0;
                    last_send = Instant::now() - SEND_INTERVAL;
                }
                HidCmd::SetHold(x) => {
                    hold = x;
                    logs.push(format!("HID: cmd SetHold({})", hold));
                    if hold {
                        let (_ok, _fail) = hid_send_out(&devices, 0, &logs);
                        last_sent_intensity = 0;
                    }
                }
                HidCmd::ReopenDevices => {
                    logs.push("HID: cmd ReopenDevices");
                    ensure_open(&mut api, &mut devices);
                }
            },
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                logs.push("HID: channel disconnected → worker exit");
                break;
            }
        }

        ensure_open(&mut api, &mut devices);

        if last_send.elapsed() >= SEND_INTERVAL {
            let out = if hold { 0 } else { desired_intensity };
            if out != last_sent_intensity {
                let (ok, fail) = hid_send_out(&devices, out, &logs);

                let now = Instant::now();
                if fail > 0
                    || ok == 0
                    || now.duration_since(last_status_log) > Duration::from_millis(900)
                {
                    logs.push(format!(
                        "HID: send intensity {} → ok={} fail={} (devs={}, hold={})",
                        out,
                        ok,
                        fail,
                        devices.len(),
                        hold
                    ));
                    last_status_log = now;
                }

                last_sent_intensity = out;
            }
            last_send = Instant::now();
        }
    }
}
