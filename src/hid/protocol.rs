pub const WW_VID: u16 = 0x4098;

pub const WW_PID_URSA_MINOR_AIRBUS_L: u16 = 0xBC27;
pub const WW_PID_URSA_MINOR_AIRBUS_R: u16 = 0xBC28;
pub const WW_PID_URSA_MINOR_FIGHTER_L: u16 = 0xBC29;
pub const WW_PID_URSA_MINOR_FIGHTER_R: u16 = 0xBC2A;
pub const WW_PID_URSA_MINOR_SPACE_L: u16 = 0xBC2B;
pub const WW_PID_URSA_MINOR_SPACE_R: u16 = 0xBC2C;

pub fn ursa_model_name(pid: u16) -> &'static str {
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

pub fn is_ursa_minor_left(pid: u16) -> bool {
    matches!(
        pid,
        WW_PID_URSA_MINOR_AIRBUS_L | WW_PID_URSA_MINOR_FIGHTER_L | WW_PID_URSA_MINOR_SPACE_L
    )
}

pub fn is_ursa_minor_right(pid: u16) -> bool {
    matches!(
        pid,
        WW_PID_URSA_MINOR_AIRBUS_R | WW_PID_URSA_MINOR_FIGHTER_R | WW_PID_URSA_MINOR_SPACE_R
    )
}

pub fn handed_selector_for_pid(pid: u16) -> u8 {
    if is_ursa_minor_right(pid) {
        0x08
    } else {
        0x07
    }
}

/// Minimum HID output report length for the simapp vibe intensity byte (body offset 7 → frame[8]).
pub const MIN_VIBE_REPORT_LEN: u16 = 14;

pub fn can_send_vibe(out_len: u16) -> bool {
    out_len >= MIN_VIBE_REPORT_LEN
}

pub fn build_simapp_vibe_frame(pid: u16, report_id: u8, out_len: u16, intensity: u8) -> Vec<u8> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn can_send_vibe_requires_min_report_len() {
        assert!(!can_send_vibe(13));
        assert!(can_send_vibe(14));
    }

    #[test]
    fn airbus_left_frame_matches_golden_bytes() {
        let frame = build_simapp_vibe_frame(WW_PID_URSA_MINOR_AIRBUS_L, 0x02, 14, 0x19);
        assert_eq!(frame.len(), 14);
        assert_eq!(frame[0], 0x02);
        assert_eq!(
            &frame[1..],
            &[0x07, 0xBF, 0x00, 0x00, 0x03, 0x49, 0x00, 0x19, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn airbus_right_uses_handed_byte_08() {
        let frame = build_simapp_vibe_frame(WW_PID_URSA_MINOR_AIRBUS_R, 0x02, 14, 0x19);
        assert_eq!(frame[1], 0x08);
        assert_eq!(frame[8], 0x19);
    }

    #[test]
    fn all_pids_have_correct_handed_selector() {
        let left_pids = [
            WW_PID_URSA_MINOR_AIRBUS_L,
            WW_PID_URSA_MINOR_FIGHTER_L,
            WW_PID_URSA_MINOR_SPACE_L,
        ];
        let right_pids = [
            WW_PID_URSA_MINOR_AIRBUS_R,
            WW_PID_URSA_MINOR_FIGHTER_R,
            WW_PID_URSA_MINOR_SPACE_R,
        ];

        for pid in left_pids {
            assert_eq!(handed_selector_for_pid(pid), 0x07, "pid=0x{pid:04X}");
        }
        for pid in right_pids {
            assert_eq!(handed_selector_for_pid(pid), 0x08, "pid=0x{pid:04X}");
        }
    }

    #[test]
    fn unknown_pid_defaults_to_left_selector() {
        assert_eq!(handed_selector_for_pid(0xFFFF), 0x07);
    }

    #[test]
    fn frame_truncates_when_out_len_is_short() {
        let frame = build_simapp_vibe_frame(WW_PID_URSA_MINOR_AIRBUS_L, 0x02, 4, 0x50);
        assert_eq!(frame, vec![0x02, 0x07, 0xBF, 0x00]);
    }

    #[test]
    fn zero_out_len_returns_empty_buffer() {
        assert!(build_simapp_vibe_frame(WW_PID_URSA_MINOR_AIRBUS_L, 0x02, 0, 0x80).is_empty());
    }

    #[test]
    fn intensity_byte_is_at_body_offset_seven() {
        for intensity in [0u8, 1, 127, 255] {
            let frame = build_simapp_vibe_frame(WW_PID_URSA_MINOR_FIGHTER_L, 0x02, 14, intensity);
            assert_eq!(frame[8], intensity);
        }
    }

    #[test]
    fn model_names_for_known_pids() {
        assert_eq!(
            ursa_model_name(WW_PID_URSA_MINOR_AIRBUS_L),
            "URSA MINOR AIRBUS L"
        );
        assert_eq!(
            ursa_model_name(WW_PID_URSA_MINOR_SPACE_R),
            "URSA MINOR SPACE R"
        );
        assert_eq!(ursa_model_name(0x0000), "UNKNOWN");
    }
}
