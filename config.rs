use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::fs;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadDirectoryChangesW, FILE_ACTION_MODIFIED, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_INFORMATION,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub mouse_low_battery_threshold: u8,
    pub numpad_low_battery_threshold: u8,
    pub relaxed_poll_interval_secs: i64,
    pub normal_poll_interval_secs: i64,
    pub critical_poll_interval_secs: i64,
    pub enable_notifications: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            mouse_low_battery_threshold: 15,
            numpad_low_battery_threshold: 15,
            relaxed_poll_interval_secs: 600,
            normal_poll_interval_secs: 300,
            critical_poll_interval_secs: 60,
            enable_notifications: true,
        }
    }
}

impl AppConfig {
    pub fn get_config_path() -> Option<PathBuf> {
        dirs::config_dir().map(|p| p.join("Battery-Tray").join("config.json"))
    }

    pub fn load() -> Self {
        let path = match Self::get_config_path() {
            Some(p) => p,
            None => return Self::default(),
        };

        if !path.exists() {
            let config = Self::default();
            let _ = config.save();
            return config;
        }

        match fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_else(|_| Self::default()),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(path) = Self::get_config_path() {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let data = serde_json::to_string_pretty(self)?;
            fs::write(path, data)?;
        }
        Ok(())
    }

    pub fn spawn_auto_reload_watcher(shared_config: Arc<std::sync::RwLock<AppConfig>>) {
        thread::spawn(move || {
            let config_path = match Self::get_config_path() {
                Some(p) => p,
                None => return,
            };

            let dir_path = config_path.parent().unwrap_or(Path::new(".")).to_path_buf();
            let file_name = config_path
                .file_name()
                .unwrap_or(OsStr::new("config.json"))
                .to_os_string();

            let dir_wstr: Vec<u16> = dir_path
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();

            let h_dir = unsafe {
                CreateFileW(
                    windows::core::PCWSTR(dir_wstr.as_ptr()),
                    FILE_LIST_DIRECTORY.0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    None,
                    OPEN_EXISTING,
                    FILE_FLAG_BACKUP_SEMANTICS,
                    HANDLE::default(),
                )
            };

            let h_dir = match h_dir {
                Ok(h) if h.0 != INVALID_HANDLE_VALUE.0 => h,
                _ => {
                    eprintln!("Failed to open directory handle for file watching.");
                    return;
                }
            };

            let mut buffer = [0u8; 1024];
            let mut bytes_returned = 0u32;

            loop {
                // Suspends thread until Windows fires a file system change notification
                let success = unsafe {
                    ReadDirectoryChangesW(
                        h_dir,
                        buffer.as_mut_ptr() as *mut _,
                        buffer.len() as u32,
                        false,
                        FILE_NOTIFY_CHANGE_LAST_WRITE,
                        Some(&mut bytes_returned),
                        None,
                        None,
                    )
                };

                if success.is_err() {
                    break;
                }

                let mut offset = 0;
                loop {
                    let info = unsafe {
                        &*(buffer.as_ptr().add(offset) as *const FILE_NOTIFY_INFORMATION)
                    };

                    let name_slice = unsafe {
                        std::slice::from_raw_parts(
                            info.FileName.as_ptr(),
                            (info.FileNameLength / 2) as usize,
                        )
                    };
                    let changed_file = std::ffi::OsString::from_wide(name_slice);

                    if changed_file == file_name && info.Action == FILE_ACTION_MODIFIED {
                        // Delay briefly to allow text editors to complete file flush operations
                        thread::sleep(std::time::Duration::from_millis(50));

                        let new_cfg = Self::load();
                        if let Ok(mut cfg) = shared_config.write() {
                            *cfg = new_cfg;
                        }
                        break;
                    }

                    if info.NextEntryOffset == 0 {
                        break;
                    }
                    offset += info.NextEntryOffset as usize;
                }
            }

            unsafe {
                let _ = CloseHandle(h_dir);
            }
        });
    }
}