use eframe::egui;
use crate::config::AppConfig;
use std::panic;

pub struct SettingsWindow {
    config: AppConfig,
}

impl SettingsWindow {
    pub fn open(config: AppConfig) {
        // Intercept panics to display an explicit Windows dialog
        panic::set_hook(Box::new(|info| {
            let msg = format!("Settings Window Panic:\n{}", info);
            eprintln!("{}", msg);
            unsafe {
                use windows::core::PCWSTR;
                use windows::Win32::UI::WindowsAndMessaging::*;
                let title: Vec<u16> = "Battery Tray Error\0".encode_utf16().collect();
                let text: Vec<u16> = format!("{}\0", msg).encode_utf16().collect();
                let _ = MessageBoxW(
                    None,
                    PCWSTR(text.as_ptr()),
                    PCWSTR(title.as_ptr()),
                    MB_OK | MB_ICONERROR,
                );
            }
        }));

        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title("Battery Tray Settings")
                .with_inner_size([420.0, 380.0])
                .with_resizable(true),
            ..Default::default()
        };

        if let Err(err) = eframe::run_native(
            "Battery Tray Settings",
            options,
            Box::new(|_cc| Box::new(SettingsWindow { config })),
        ) {
            unsafe {
                use windows::core::PCWSTR;
                use windows::Win32::UI::WindowsAndMessaging::*;
                let title: Vec<u16> = "eframe Launch Error\0".encode_utf16().collect();
                let text: Vec<u16> = format!("{:?}\0", err).encode_utf16().collect();
                let _ = MessageBoxW(
                    None,
                    PCWSTR(text.as_ptr()),
                    PCWSTR(title.as_ptr()),
                    MB_OK | MB_ICONERROR,
                );
            }
        }
    }
}

impl eframe::App for SettingsWindow {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Battery Tray Settings");
            ui.separator();

            let mut changed = false;

            ui.add_space(8.0);
            ui.label(egui::RichText::new("Notifications").strong());
            if ui.checkbox(&mut self.config.enable_notifications, "Enable Low Battery Notifications").changed() {
                changed = true;
            }

            ui.add_space(8.0);
            ui.label(egui::RichText::new("Low Battery Thresholds (%)").strong());
            ui.horizontal(|ui| {
                ui.label("Mouse Threshold:");
                if ui.add(egui::Slider::new(&mut self.config.mouse_low_battery_threshold, 5..=50).suffix("%")).changed() {
                    changed = true;
                }
            });

            ui.horizontal(|ui| {
                ui.label("Numpad Threshold:");
                if ui.add(egui::Slider::new(&mut self.config.numpad_low_battery_threshold, 5..=50).suffix("%")).changed() {
                    changed = true;
                }
            });

            ui.add_space(8.0);
            ui.label(egui::RichText::new("Polling Intervals (Seconds)").strong());
            ui.horizontal(|ui| {
                ui.label("Normal Interval:");
                if ui.add(egui::Slider::new(&mut self.config.normal_poll_interval_secs, 10..=600).suffix("s")).changed() {
                    changed = true;
                }
            });

            ui.horizontal(|ui| {
                ui.label("Critical Interval:");
                if ui.add(egui::Slider::new(&mut self.config.critical_poll_interval_secs, 5..=300).suffix("s")).changed() {
                    changed = true;
                }
            });

            ui.horizontal(|ui| {
                ui.label("Relaxed Interval:");
                if ui.add(egui::Slider::new(&mut self.config.relaxed_poll_interval_secs, 60..=3600).suffix("s")).changed() {
                    changed = true;
                }
            });

            ui.add_space(16.0);
            ui.separator();
            
            if changed {
                let _ = self.config.save();
            }

            if ui.button("Save & Close").clicked() {
                let _ = self.config.save();
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });
    }
}