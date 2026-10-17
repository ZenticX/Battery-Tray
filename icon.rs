// src/icon.rs

use crate::battery::DeviceBatteryStatus;

// Defines which physical peripheral an icon instance represents.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DeviceType {
    Mouse,
    Numpad,
}

// Icon dimensions in pixels (32x32 standard system tray size).
const W: u32 = 32;
const H: u32 = 32;

/// Returns the icon canvas dimensions (width, height).
pub fn icon_dim() -> (u32, u32) {
    (W, H)
}

/// Generates raw RGBA bitmap byte data for a device's tray icon.
pub fn build_icon_rgba(status: DeviceBatteryStatus, dev_type: DeviceType) -> Vec<u8> {
    // 32x32 pixels * 4 channels = 4,096 bytes (already zero-initialized/transparent)
    let mut buf = vec![0u8; (W * H * 4) as usize];

    let offline = [100, 100, 100, 255]; // Gray fill when device is disconnected
    let charging = [0, 210, 255, 255];  // Bright Cyan fill when charging

    // Visual identifier frame color: White for Razer Mouse, Slate Gray for Stars21-S Numpad (visible on dark taskbars)
    let frame = match dev_type {
        DeviceType::Mouse => [255, 255, 255, 255], // Pure White
        DeviceType::Numpad => [160, 160, 160, 255], // Slate Gray
    };

    // Color code battery level: Red (<20%), Orange (<60%), Green (>=60%), Cyan (Charging), Gray (Offline)
    let fill_color = match status {
        DeviceBatteryStatus::Discharging(p) if p < 20 => [255, 60, 60, 255],  // Red
        DeviceBatteryStatus::Discharging(p) if p < 60 => [255, 165, 0, 255], // Orange
        DeviceBatteryStatus::Discharging(_) => [70, 220, 90, 255],            // Green
        DeviceBatteryStatus::Charging => charging,                             // Cyan
        DeviceBatteryStatus::Offline => offline,                               // Gray
    };

    let draw_pixel = |buf: &mut [u8], x: u32, y: u32, c: [u8; 4]| {
        if x < W && y < H {
            let i = ((y * W + x) * 4) as usize;
            buf[i..i + 4].copy_from_slice(&c);
        }
    };

    // 1. Draw 2px thick top and bottom frame lines
    for x in 3..27 {
        draw_pixel(&mut buf, x, 10, frame);
        draw_pixel(&mut buf, x, 11, frame);
        draw_pixel(&mut buf, x, 20, frame);
        draw_pixel(&mut buf, x, 21, frame);
    }

    // 2. Draw 2px thick left and right frame lines
    for y in 10..22 {
        draw_pixel(&mut buf, 3, y, frame);
        draw_pixel(&mut buf, 4, y, frame);
        draw_pixel(&mut buf, 25, y, frame);
        draw_pixel(&mut buf, 26, y, frame);
    }

    // 3. Draw battery positive terminal nub on right side
    for y in 13..19 {
        for x in 27..29 {
            draw_pixel(&mut buf, x, y, frame);
        }
    }

// 4. Fill inner battery area based on state
    match status {
        DeviceBatteryStatus::Discharging(p) => {
            const MAX_INNER_W: f32 = 19.0;
            let fill_width = ((p as f32 / 100.0) * MAX_INNER_W).round() as u32;

            for y in 12..20 {
                for x in 5..(5 + fill_width) {
                    draw_pixel(&mut buf, x, y, fill_color);
                }
            }
        }
        DeviceBatteryStatus::Charging => {
            for y in 12..20 {
                for x in 5..24 {
                    draw_pixel(&mut buf, x, y, charging);
                }
            }
        }
        DeviceBatteryStatus::Offline => {
            for y in 12..20 {
                for x in 5..24 {
                    draw_pixel(&mut buf, x, y, offline);
                }
            }
        }
    }

    buf
}