// src/battery.rs

use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Devices::DeviceAndDriverInstallation::*;
use windows::Win32::Devices::HumanInterfaceDevice::*;
use windows::Win32::Foundation::*;
use windows::Win32::Storage::FileSystem::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceBatteryStatus {
    Discharging(u8),
    Charging,
    Offline,
}

pub struct SystemBatteryStatus {
    pub mouse: DeviceBatteryStatus,
    pub numpad: DeviceBatteryStatus,
}

const VID_RAZER: u16 = 0x1532;
const PID_VIPER_V3_PRO_WIRED: u16 = 0x00C0;
const PID_VIPER_V3_PRO_WIRELESS: u16 = 0x00C1;
const PID_RAZER_DONGLE_1: u16 = 0x00B6;
const PID_RAZER_DONGLE_2: u16 = 0x00B7;

const RAZER_TID: u8 = 0x1F;
const REPORT_LEN: usize = 91;

const VID_STARS21: u16 = 0x342D;
const PID_STARS21: u16 = 0xE543;

/// RAII wrapper to guarantee handles are closed on any exit path.
struct SafeHandle(HANDLE);

impl SafeHandle {
    fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for SafeHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() && self.0 != INVALID_HANDLE_VALUE {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

struct PreparsedDataWrapper(PHIDP_PREPARSED_DATA);

impl Drop for PreparsedDataWrapper {
    fn drop(&mut self) {
        unsafe {
            let _ = HidD_FreePreparsedData(self.0);
        }
    }
}

pub fn read_battery_status() -> SystemBatteryStatus {
    let mut mouse = DeviceBatteryStatus::Offline;
    let mut numpad = DeviceBatteryStatus::Offline;

    if let Ok(paths) = enumerate_hid_paths() {
        for path in paths {
            // Check Razer mouse only on target paths
            if mouse == DeviceBatteryStatus::Offline && is_target_razer(&path) {
                mouse = try_query_razer(&path);
            }

            // Check Stars21 numpad
            if numpad == DeviceBatteryStatus::Offline && is_target_stars21(&path) {
                numpad = try_query_stars21(&path);
            }

            // Exit early if both devices are successfully read
            if mouse != DeviceBatteryStatus::Offline && numpad != DeviceBatteryStatus::Offline {
                break;
            }
        }
    }

    SystemBatteryStatus { mouse, numpad }
}

fn is_target_razer(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.contains(&format!("vid_{:04x}", VID_RAZER))
        && (lower.contains(&format!("pid_{:04x}", PID_VIPER_V3_PRO_WIRED))
            || lower.contains(&format!("pid_{:04x}", PID_VIPER_V3_PRO_WIRELESS))
            || lower.contains(&format!("pid_{:04x}", PID_RAZER_DONGLE_1))
            || lower.contains(&format!("pid_{:04x}", PID_RAZER_DONGLE_2)))
}

fn is_target_stars21(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.contains(&format!("vid_{:04x}", VID_STARS21))
        && lower.contains(&format!("pid_{:04x}", PID_STARS21))
}

fn enumerate_hid_paths() -> Result<Vec<String>, String> {
    unsafe {
        let hid_guid = HidD_GetHidGuid();

        let dev_info = SetupDiGetClassDevsW(
            Some(&hid_guid),
            PCWSTR::null(),
            None,
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
        .map_err(|e| format!("SetupDiGetClassDevs: {e}"))?;

        let mut paths = Vec::new();
        let mut idx = 0u32;
        loop {
            let mut iface = SP_DEVICE_INTERFACE_DATA::default();
            iface.cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
            if SetupDiEnumDeviceInterfaces(dev_info, None, &hid_guid, idx, &mut iface).is_err() {
                break;
            }
            idx += 1;

            let mut required: u32 = 0;
            let _ = SetupDiGetDeviceInterfaceDetailW(
                dev_info,
                &iface,
                None,
                0,
                Some(&mut required),
                None,
            );
            if required == 0 {
                continue;
            }

            let mut buf = vec![0u8; required as usize];
            let detail = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
            (*detail).cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;

            if SetupDiGetDeviceInterfaceDetailW(
                dev_info,
                &iface,
                Some(detail),
                required,
                None,
                None,
            )
            .is_err()
            {
                continue;
            }

            let path_ptr = (*detail).DevicePath.as_ptr();
            let path = wide_to_string(path_ptr);
            paths.push(path);
        }
        let _ = SetupDiDestroyDeviceInfoList(dev_info);
        Ok(paths)
    }
}

unsafe fn wide_to_string(p: *const u16) -> String {
    let mut len = 0usize;
    while *p.add(len) != 0 {
        len += 1;
    }
    let slice = std::slice::from_raw_parts(p, len);
    OsString::from_wide(slice).to_string_lossy().into_owned()
}

fn to_wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

fn try_query_razer(path: &str) -> DeviceBatteryStatus {
    unsafe {
        let wide = to_wide(path);
        let raw_handle = CreateFileW(
            PCWSTR(wide.as_ptr()),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
        .ok();

        let handle = match raw_handle {
            Some(h) if !h.is_invalid() => SafeHandle(h),
            _ => return DeviceBatteryStatus::Offline,
        };

        let mut preparsed: PHIDP_PREPARSED_DATA = PHIDP_PREPARSED_DATA::default();
        if !HidD_GetPreparsedData(handle.raw(), &mut preparsed).as_bool() {
            return DeviceBatteryStatus::Offline;
        }

        let _preparsed_guard = PreparsedDataWrapper(preparsed);

        let mut caps: HIDP_CAPS = std::mem::zeroed();
        let cap_status = HidP_GetCaps(preparsed, &mut caps);

        if cap_status != HIDP_STATUS_SUCCESS
            || caps.UsagePage != 0x01
            || caps.Usage != 0x02
            || (caps.FeatureReportByteLength as usize) < REPORT_LEN
        {
            return DeviceBatteryStatus::Offline;
        }

        let mut req = [0u8; REPORT_LEN];
        req[0] = 0x00;
        req[2] = RAZER_TID;
        req[6] = 0x02;
        req[7] = 0x07;
        req[8] = 0x80; // Correct Razer Battery Command

        let mut crc: u8 = 0;
        for i in 3..=88 {
            crc ^= req[i];
        }
        req[89] = crc;

        let deadline = Instant::now() + Duration::from_millis(300);
        let mut consecutive_failures = 0;
        const MAX_CONSECUTIVE_FAILURES: u32 = 3;

        while Instant::now() < deadline {
            let set_ok = HidD_SetFeature(handle.raw(), req.as_ptr() as *mut _, REPORT_LEN as u32);
            if !set_ok.as_bool() {
                consecutive_failures += 1;
                if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                    break;
                }
                std::thread::yield_now();
                continue;
            }

            let mut resp = [0u8; REPORT_LEN];
            let get_ok = HidD_GetFeature(handle.raw(), resp.as_mut_ptr() as *mut _, REPORT_LEN as u32);
            if !get_ok.as_bool() {
                consecutive_failures += 1;
                if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                    break;
                }
                std::thread::yield_now();
                continue;
            }

            consecutive_failures = 0;

            // 0x02 = Successful Response Flag
            if resp[1] == 0x02 {
                let raw = resp[10];
                if raw == 0 {
                    break;
                }
                // Convert 0-255 scale to percentage 0-100%
                let pct = (((raw as u32) * 100) / 255) as u8;
                return DeviceBatteryStatus::Discharging(pct.min(100));
            }

            // Status 0x01 = Command pending on wireless link; keep polling until deadline
            if resp[1] != 0x01 {
                break;
            }

            std::thread::yield_now();
        }

        DeviceBatteryStatus::Offline
    }
}

fn try_query_stars21(path: &str) -> DeviceBatteryStatus {
    unsafe {
        let wide = to_wide(path);

        let raw_handle = CreateFileW(
            PCWSTR(wide.as_ptr()),
            GENERIC_READ.0 | GENERIC_WRITE.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
        .ok();

        let handle = match raw_handle {
            Some(h) if !h.is_invalid() => SafeHandle(h),
            _ => return DeviceBatteryStatus::Offline,
        };

        let mut preparsed: PHIDP_PREPARSED_DATA = PHIDP_PREPARSED_DATA::default();
        if !HidD_GetPreparsedData(handle.raw(), &mut preparsed).as_bool() {
            return DeviceBatteryStatus::Offline;
        }

        let _preparsed_guard = PreparsedDataWrapper(preparsed);

        let mut caps: HIDP_CAPS = std::mem::zeroed();
        let cap_status = HidP_GetCaps(preparsed, &mut caps);

        if cap_status != HIDP_STATUS_SUCCESS
            || (caps.UsagePage != 0x01 && caps.UsagePage != 0x0C && caps.UsagePage != 0xFF60)
        {
            return DeviceBatteryStatus::Offline;
        }

        let mut command_buf = [0u8; 32];
        command_buf[0] = 0x00;
        command_buf[1] = 0x02;
        let _ = HidD_SetFeature(handle.raw(), command_buf.as_ptr() as *mut _, command_buf.len() as u32);

        let mut input_buf = [0u8; 64];
        input_buf[0] = 0x00;

        let read_ok = HidD_GetInputReport(handle.raw(), input_buf.as_mut_ptr() as *mut _, input_buf.len() as u32);

        if read_ok.as_bool() {
            for &idx in &[1, 2, 3, 4] {
                let val = input_buf[idx];

                if val > 0 && val <= 100 {
                    return DeviceBatteryStatus::Discharging(val);
                }

                if val == 0xFF || val == 0xFE || val > 100 {
                    return DeviceBatteryStatus::Charging;
                }
            }
        }

        DeviceBatteryStatus::Offline
    }
}