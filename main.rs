#![windows_subsystem = "windows"]

mod battery;
mod config;
mod icon;
mod popup;
mod ui;

use std::env;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;
use std::thread;


use battery::DeviceBatteryStatus;
use config::AppConfig;
use icon::DeviceType;
use muda::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use winrt_notification::{Duration, Toast};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2;
use windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext;
use windows::Win32::UI::WindowsAndMessaging::*;

// Sentinel values used for encoding device states into atomic integers for thread safety
const STATE_OFFLINE: i32 = -1;
const STATE_CHARGING: i32 = -2;

// Custom application window message IDs
const WM_APP_REFRESH: u32 = WM_APP + 1;
const WM_APP_TRAY_LCLICK: u32 = WM_APP + 2;
const WM_APP_MENU: u32 = WM_APP + 3;

// Global thread-safe handle storage for the message-only window
static MESSAGE_HWND: AtomicI32 = AtomicI32::new(0);

// De-duplication flags to prevent spamming notifications on every refresh cycle
static MOUSE_LOW_NOTIFIED: AtomicBool = AtomicBool::new(false);
static NUMPAD_LOW_NOTIFIED: AtomicBool = AtomicBool::new(false);

/// Stores the raw handle of the hidden message-only window thread-safely.
fn set_msg_hwnd(hwnd: HWND) {
    MESSAGE_HWND.store(hwnd.0 as usize as i32, Ordering::Release);
}

/// Retrieves the raw handle of the hidden message-only window thread-safely.
fn get_msg_hwnd() -> HWND {
    HWND(MESSAGE_HWND.load(Ordering::Acquire) as isize as *mut _)
}

/// Window Procedure for the hidden message window (handles system power broadcasts).
unsafe extern "system" fn msg_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_POWERBROADCAST => {
            const PBT_APMRESUMESUSPEND: usize = 0x0007;
            if wparam.0 == PBT_APMRESUMESUSPEND {
                // Resume handled in main loop through timer reset if needed
            }
            LRESULT(1)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Registers the window class and creates an `HWND_MESSAGE` message-only window
/// used as an event dispatch target for background notifications.
fn create_message_window() -> HWND {
    unsafe {
        let instance = GetModuleHandleW(None).unwrap_or_default();
        let class_name: Vec<u16> = "BatteryTrayMsgClass\0".encode_utf16().collect();

        let wnd_class = WNDCLASSW {
            lpfnWndProc: Some(msg_wnd_proc),
            hInstance: instance.into(),
            lpszClassName: PCWSTR(class_name.as_ptr()),
            ..Default::default()
        };

        RegisterClassW(&wnd_class);

        let hwnd = CreateWindowExW(
            Default::default(),
            PCWSTR(class_name.as_ptr()),
            PCWSTR(class_name.as_ptr()),
            Default::default(),
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            None,
            instance,
            None,
        )
        .expect("Failed to create message window");

        set_msg_hwnd(hwnd);
        hwnd
    }
}

/// Safe RAII wrapper around raw Win32 kernel handles (Mutex, Timer, etc.).
/// Automatically closes the wrapped handle upon dropping out of scope.
struct AutoHandle(HANDLE);

impl AutoHandle {
    /// Returns the underlying raw Win32 HANDLE.
    fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for AutoHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() && self.0 != INVALID_HANDLE_VALUE {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

unsafe impl Send for AutoHandle {}
unsafe impl Sync for AutoHandle {}

/// Converts high-level `DeviceBatteryStatus` enum to an `i32` for atomic thread communication.
fn status_to_i32(status: DeviceBatteryStatus) -> i32 {
    match status {
        DeviceBatteryStatus::Discharging(pct) => pct as i32,
        DeviceBatteryStatus::Charging => STATE_CHARGING,
        DeviceBatteryStatus::Offline => STATE_OFFLINE,
    }
}

/// Converts encoded `i32` atomic state back into standard `DeviceBatteryStatus` enum.
fn i32_to_status(val: i32) -> DeviceBatteryStatus {
    match val {
        STATE_CHARGING => DeviceBatteryStatus::Charging,
        0..=100 => DeviceBatteryStatus::Discharging(val as u8),
        _ => DeviceBatteryStatus::Offline,
    }
}

/// Formats the device status for user-facing UI text elements (tooltips & popups).
fn format_status(status: DeviceBatteryStatus) -> String {
    match status {
        DeviceBatteryStatus::Discharging(pct) => format!("{}%", pct),
        DeviceBatteryStatus::Charging => "Charging".into(),
        DeviceBatteryStatus::Offline => "Offline".into(),
    }
}

/// Evaluates device status and displays a Windows toast alert if battery drops <= threshold.
/// Tracks notification state via `notified_flag` to fire only once per low-battery cycle.
fn check_and_notify_low_battery(
    status: DeviceBatteryStatus,
    device_name: &str,
    notified_flag: &AtomicBool,
    config: &AppConfig,
) {
    if !config.enable_notifications {
        return;
    }

    if let DeviceBatteryStatus::Discharging(pct) = status {
        let threshold = if device_name.contains("Viper") {
            config.mouse_low_battery_threshold
        } else {
            config.numpad_low_battery_threshold
        };

        if pct <= threshold {
            if !notified_flag.swap(true, Ordering::SeqCst) {
                let _ = Toast::new(Toast::POWERSHELL_APP_ID)
                    .title("Low Battery Warning")
                    .text1(&format!("{} is down to {}%!", device_name, pct))
                    .duration(Duration::Short)
                    .show();
            }
        } else {
            notified_flag.store(false, Ordering::SeqCst);
        }
    } else {
        notified_flag.store(false, Ordering::SeqCst);
    }
}

/// Calculates the dynamic polling delay in seconds based on current device states.
fn calculate_next_interval(
    mouse: DeviceBatteryStatus,
    numpad: DeviceBatteryStatus,
    cfg: &AppConfig,
) -> i64 {
    let get_level = |status| match status {
        DeviceBatteryStatus::Discharging(pct) => Some(pct),
        _ => None,
    };

    let m_pct = get_level(mouse);
    let n_pct = get_level(numpad);

    if m_pct.map_or(false, |p| p <= cfg.mouse_low_battery_threshold)
        || n_pct.map_or(false, |p| p <= cfg.numpad_low_battery_threshold)
    {
        return cfg.critical_poll_interval_secs;
    }

    let m_relaxed = m_pct.map_or(true, |p| p >= 90);
    let n_relaxed = n_pct.map_or(true, |p| p >= 90);
    if m_relaxed && n_relaxed {
        return cfg.relaxed_poll_interval_secs;
    }

    cfg.normal_poll_interval_secs
}

fn main() {
    let args: Vec<String> = env::args().collect();

   // 0. Bypasses singleton mutex and tray initialization completely if spawned for Settings UI
    if args.iter().any(|arg| arg == "--settings") {
        let _ = std::panic::catch_unwind(|| {
            let cfg = AppConfig::load();
            ui::SettingsWindow::open(cfg);
        });
        return;
    }

    // 1. Load configuration into thread-safe shared state and start file watcher
    let config = Arc::new(std::sync::RwLock::new(AppConfig::load()));
    AppConfig::spawn_auto_reload_watcher(config.clone());

    // 2. Configure High-DPI support to ensure sharp tray icons across monitors
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_MULTITHREADED,
        );
    }

    // 3. Enforce single application instance using named system mutex for the tray process
    let _singleton_guard = match acquire_singleton() {
        Some(h) => h,
        None => return,
    };

    // 4. Create hidden message-only window for message dispatching
    let _msg_hwnd = create_message_window();

    // 5. Construct context menu items for system tray icons
    let menu = Menu::new();
    let item_refresh = MenuItem::new("Refresh now", true, None);
    let edit_config_item = MenuItem::new("Edit Settings", true, None);
    let item_exit = MenuItem::new("Exit", true, None);

    menu.append(&item_refresh).unwrap();
    menu.append(&edit_config_item).unwrap();
    menu.append(&item_exit).unwrap();

    let id_refresh = item_refresh.id().clone();
    let id_edit_config = edit_config_item.id().clone();
    let id_exit = item_exit.id().clone();

    // Shared atomic state tracking battery status across background & UI processes
    let mouse_state = Arc::new(AtomicI32::new(STATE_OFFLINE));
    let numpad_state = Arc::new(AtomicI32::new(STATE_OFFLINE));

    // 6. Initialize System Tray Icons using `tray_icon`
    let tray_mouse = TrayIconBuilder::new()
        .with_menu(Box::new(menu.clone()))
        .with_menu_on_left_click(false)
        .with_tooltip("Viper V3 Pro: Checking...")
        .with_icon(build_icon(DeviceBatteryStatus::Offline, DeviceType::Mouse))
        .build()
        .expect("failed to build mouse tray");

    let tray_numpad = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip("Stars21-S Numpad: Checking...")
        .with_icon(build_icon(DeviceBatteryStatus::Offline, DeviceType::Numpad))
        .build()
        .expect("failed to build numpad tray");

    let mouse_id = tray_mouse.id().clone();

    // 7. Create Win32 Waitable Timer for non-blocking background polling schedule
    let raw_timer = unsafe { CreateWaitableTimerW(None, false, PCWSTR::null()) }
        .expect("CreateWaitableTimer failed");
    let timer = Arc::new(AutoHandle(raw_timer));

    // Arm timer immediately to execute an initial status query on startup
    arm_timer(&timer, 0);

    // 8. Spawn dedicated worker thread for HID hardware polling
    {
        let mouse_state = mouse_state.clone();
        let numpad_state = numpad_state.clone();
        let timer = timer.clone();
        let config = config.clone();

        thread::spawn(move || loop {
            wait_handle(&timer);

            let status = battery::read_battery_status();
	    println!("DEBUG - Mouse Status: {:?}, Numpad Status: {:?}", status.mouse, status.numpad);

            mouse_state.store(status_to_i32(status.mouse), Ordering::Release);
            numpad_state.store(status_to_i32(status.numpad), Ordering::Release);

            let hwnd = get_msg_hwnd();
            if !hwnd.0.is_null() {
                unsafe {
                    let _ = PostMessageW(hwnd, WM_APP_REFRESH, WPARAM(0), LPARAM(0));
                }
            }

            let cfg = config.read().unwrap();
            let next_secs = calculate_next_interval(status.mouse, status.numpad, &cfg);
            arm_timer(&timer, next_secs);
        });
    }

    // 9. Register left-click event handler for tray icons
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        if let TrayIconEvent::Click {
            id,
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            let is_mouse = id == mouse_id;
            let hwnd = get_msg_hwnd();
            if !hwnd.0.is_null() {
                unsafe {
                    let param = if is_mouse { 1 } else { 2 };
                    let _ = PostMessageW(hwnd, WM_APP_TRAY_LCLICK, WPARAM(param), LPARAM(0));
                }
            }
        }
    }));

    // 10. Register context menu event handler
    let menu_event = Arc::new(AtomicI32::new(0));
    {
        let id_refresh = id_refresh.clone();
        let id_edit_config = id_edit_config.clone();
        let id_exit = id_exit.clone();
        let menu_event = menu_event.clone();

        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let v = if event.id == id_refresh {
                1
            } else if event.id == id_exit {
                2
            } else if event.id == id_edit_config {
                3
            } else {
                0
            };

            if v != 0 {
                menu_event.store(v, Ordering::Release);
                let hwnd = get_msg_hwnd();
                if !hwnd.0.is_null() {
                    unsafe {
                        let _ = PostMessageW(hwnd, WM_APP_MENU, WPARAM(0), LPARAM(0));
                    }
                }
            }
        }));
    }

    // 11. Win32 Main Message Loop (Runs continuously on primary thread)
    unsafe {
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            match msg.message {
                // UI Refresh event dispatched by background polling thread
                WM_APP_REFRESH => {
                    let m_status = i32_to_status(mouse_state.load(Ordering::Acquire));
                    let n_status = i32_to_status(numpad_state.load(Ordering::Acquire));

                    // Update Tray Icons & Tooltips
                    let _ = tray_mouse.set_icon(Some(build_icon(m_status, DeviceType::Mouse)));
                    let _ = tray_mouse.set_tooltip(Some(format!(
                        "Viper V3 Pro: {}",
                        format_status(m_status)
                    )));

                    let _ = tray_numpad.set_icon(Some(build_icon(n_status, DeviceType::Numpad)));
                    let _ = tray_numpad.set_tooltip(Some(format!(
                        "Stars21-S Numpad: {}",
                        format_status(n_status)
                    )));

                    // Process low-battery desktop notifications
                    let cfg = config.read().unwrap();
                    check_and_notify_low_battery(m_status, "Viper V3 Pro", &MOUSE_LOW_NOTIFIED, &cfg);
                    check_and_notify_low_battery(n_status, "Stars21-S Numpad", &NUMPAD_LOW_NOTIFIED, &cfg);
                }

                // Tray icon left-click event: displays floating status popup window near cursor
                WM_APP_TRAY_LCLICK => {
                    let clicked_mouse = msg.wParam.0 == 1;
                    let m_status = i32_to_status(mouse_state.load(Ordering::Acquire));
                    let n_status = i32_to_status(numpad_state.load(Ordering::Acquire));

                    let popup_text = if clicked_mouse {
                        format!("Viper V3 Pro: {}", format_status(m_status))
                    } else {
                        format!("Stars21-S Numpad: {}", format_status(n_status))
                    };

                    popup::toggle_popup(&popup_text);
                }

                // Context menu actions handler
                WM_APP_MENU => {
                    let action = menu_event.swap(0, Ordering::Acquire);
                    match action {
                        1 => arm_timer(&timer, 0), // Refresh now
                        2 => {                     // Exit Application
                            PostQuitMessage(0);
                            std::process::exit(0);
                        }
  			3 => { // Edit Settings
    				if let Ok(exe) = env::current_exe() {
       				let _ = Command::new(exe)
            			.arg("--settings")
            			.spawn();
    }
}
                        _ => {}
                    }
                }

                // Native Windows Power Broadcast Listener (Wake up from sleep)
                WM_POWERBROADCAST => {
                    const PBT_APMRESUMESUSPEND: usize = 0x0007;
                    if msg.wParam.0 == PBT_APMRESUMESUSPEND {
                        arm_timer(&timer, 0);
                    }
                }

                // Standard Win32 Message Dispatching
                _ => {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }

        windows::Win32::System::Com::CoUninitialize();
    }
}

/// Configures Win32 waitable timer to fire after specified duration in seconds.
fn arm_timer(h: &AutoHandle, secs: i64) {
    let due_ns: i64 = if secs <= 0 { -1 } else { -secs * 10_000_000 };
    unsafe {
        let _ = SetWaitableTimer(h.raw(), &due_ns, 0, None, None, false);
    }
}

/// Helper to perform zero-CPU blocking wait on a Win32 handle.
fn wait_handle(h: &AutoHandle) {
    unsafe {
        let _ = WaitForSingleObject(h.raw(), INFINITE);
    }
}

/// Generates an icon struct suitable for `tray_icon` based on device type and status.
fn build_icon(status: DeviceBatteryStatus, dev_type: DeviceType) -> Icon {
    let (w, h) = icon::icon_dim();
    let rgba = icon::build_icon_rgba(status, dev_type);
    Icon::from_rgba(rgba, w, h).expect("icon")
}

/// Attempts to acquire a named mutex to ensure only one app instance runs at a time.
fn acquire_singleton() -> Option<AutoHandle> {
    unsafe {
        let h = match CreateMutexW(None, true, w!("Local\\ViperTray-singleton")) {
            Ok(h) => h,
            Err(_) => return None,
        };
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(h);
            return None;
        }
        Some(AutoHandle(h))
    }
}