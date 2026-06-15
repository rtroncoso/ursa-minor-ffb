use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

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

use crate::LogBuffer;

pub fn hid_query_caps_from_path(path_utf8: &str, logs: &LogBuffer) -> Option<(u16, u8)> {
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
