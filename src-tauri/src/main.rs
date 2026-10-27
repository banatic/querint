#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod engine;
mod settings;
mod spooler;

use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use tauri::image::Image;
use tauri::tray::{MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{
    AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, Rect, State, WebviewWindow, WindowEvent,
};
use tauri_plugin_notification::NotificationExt;

use engine::{Engine, Snapshot};
use settings::Settings;

const WIDTH: f64 = 320.0;
const TICK: Duration = Duration::from_millis(700);
const REACH_EVERY: Duration = Duration::from_secs(4);


const ICON_PAUSED: &[u8] = include_bytes!("../icons/printer-queue-32.png");
const ICON_PRINTING: &[u8] = include_bytes!("../icons/printer-queue-printing-32.png");
const ICON_IDLE: &[u8] = include_bytes!("../icons/printer-queue-idle-32.png");

struct Ui {
    /// Tray icon top-centre in physical pixels; the window sits above it.
    anchor: Option<(f64, f64)>,
    hidden_at: Instant,
    last: Snapshot,
}

struct AppState {
    engine: Mutex<Engine>,
    ui: Mutex<Ui>,
}

fn publish(app: &AppHandle, force: bool) {
    let state = app.state::<AppState>();
    let snap = state.engine.lock().unwrap().snapshot();
    {
        // Tray updates block on the main thread, so never hold a lock across them.
        let mut ui = state.ui.lock().unwrap();
        if !force && snap == ui.last {
            return;
        }
        ui.last = snap.clone();
    }
    if let Some(tray) = app.tray_by_id("main") {
        let (icon, tip) = if snap.active > 0 {
            (ICON_PRINTING, format!("Querint · {}건 인쇄 중", snap.active))
        } else if snap.held > 0 {
            (ICON_PAUSED, format!("Querint · {}건 보류", snap.held))
        } else {
            (ICON_IDLE, "Querint".to_string())
        };
        let _ = tray.set_icon(Image::from_bytes(icon).ok());
        let _ = tray.set_tooltip(Some(tip));
    }
    let _ = app.emit("state", &snap);
}

fn act(app: &AppHandle, f: impl FnOnce(&mut Engine)) {
    {
        let state = app.state::<AppState>();
        let mut engine = state.engine.lock().unwrap();
        f(&mut engine);
        engine.tick();
    }
    publish(app, false);
}

fn place(win: &WebviewWindow, anchor: (f64, f64)) {
    let Ok(size) = win.outer_size() else { return };
    let scale = win.scale_factor().unwrap_or(1.0);
    let margin = 8.0 * scale;
    let monitor = win.monitor_from_point(anchor.0, anchor.1).ok().flatten().or_else(|| win.primary_monitor().ok().flatten());
    let Some(monitor) = monitor else { return };
    let area = monitor.work_area();
    let (left, top) = (area.position.x as f64, area.position.y as f64);
    let (right, bottom) = (left + area.size.width as f64, top + area.size.height as f64);
    let (w, h) = (size.width as f64, size.height as f64);
    let x = (anchor.0 - w / 2.0).clamp(left + margin, (right - w - margin).max(left + margin));
    // Taskbar at the top puts the tray above the work area.
    let y = if anchor.1 < top { top + margin } else { (bottom - h - margin).max(top) };
    let _ = win.set_position(PhysicalPosition::new(x.round() as i32, y.round() as i32));
}

fn toggle(app: &AppHandle, rect: Rect) {
    let Some(win) = app.get_webview_window("main") else { return };
    let state = app.state::<AppState>();
    if win.is_visible().unwrap_or(false) {
        hide(win);
        return;
    }
    // The click that blurred (and hid) the window is the same click on the tray icon.
    if state.ui.lock().unwrap().hidden_at.elapsed() < Duration::from_millis(250) {
        return;
    }
    let scale = win.scale_factor().unwrap_or(1.0);
    let pos = rect.position.to_physical::<f64>(scale);
    let size = rect.size.to_physical::<f64>(scale);
    show_at(app, &win, (pos.x + size.width / 2.0, pos.y));
}

fn show_at(app: &AppHandle, win: &WebviewWindow, anchor: (f64, f64)) {
    app.state::<AppState>().ui.lock().unwrap().anchor = Some(anchor);
    place(win, anchor);
    publish(app, true);
    let _ = win.show();
    let _ = win.set_focus();
    let _ = win.emit("visible", true);
}

/// Launching the exe again opens the window at the last tray position (or the bottom-right corner).
fn show_again(app: &AppHandle) {
    let Some(win) = app.get_webview_window("main") else { return };
    let anchor = app.state::<AppState>().ui.lock().unwrap().anchor;
    let anchor = anchor.or_else(|| {
        let area = win.primary_monitor().ok().flatten()?.work_area().clone();
        Some(((area.position.x + area.size.width as i32) as f64, (area.position.y + area.size.height as i32) as f64))
    });
    if let Some(anchor) = anchor {
        show_at(app, &win, anchor);
    }
}

#[tauri::command]
fn get_state(state: State<AppState>) -> Snapshot {
    state.engine.lock().unwrap().snapshot()
}

#[tauri::command]
fn release(app: AppHandle, printer: String, job: Option<u32>) {
    act(&app, |e| e.release(&printer, job));
}

#[tauri::command]
fn release_all(app: AppHandle) {
    act(&app, |e| e.release_all());
}

#[tauri::command]
fn stop(app: AppHandle, printer: Option<String>) {
    act(&app, |e| match printer {
        Some(p) => e.stop(&p),
        None => e.stop_all(),
    });
}

#[tauri::command]
fn delete_job(app: AppHandle, printer: String, job: u32) {
    act(&app, |e| e.delete(&printer, job));
}

#[tauri::command]
fn set_managed(app: AppHandle, printer: String, managed: bool) {
    act(&app, |e| e.set_managed(&printer, managed));
}

#[tauri::command]
fn set_hold(app: AppHandle, printer: String, hold: bool) {
    act(&app, |e| e.set_bypass(&printer, !hold));
}

#[tauri::command]
fn set_option(app: AppHandle, key: String, value: bool) {
    act(&app, |e| {
        match key.as_str() {
            "autostart" => {
                if settings::set_autostart(value) {
                    e.settings.autostart = value;
                }
            }
            "notify" => e.settings.notify = value,
            _ => return,
        }
        e.settings.save();
    });
}

#[tauri::command]
fn fit(window: WebviewWindow, state: State<AppState>, height: f64) {
    let _ = window.set_size(LogicalSize::new(WIDTH, height.ceil()));
    if let Some(anchor) = state.ui.lock().unwrap().anchor {
        place(&window, anchor);
    }
}

/// The page pauses its animation while hidden; WebView2 keeps a hidden window's frames running.
#[tauri::command]
fn hide(window: WebviewWindow) {
    let _ = window.hide();
    let _ = window.emit("visible", false);
}

#[tauri::command]
fn quit(app: AppHandle) {
    app.exit(0);
}

fn spawn_ticker(app: AppHandle) {
    thread::spawn(move || loop {
        app.state::<AppState>().engine.lock().unwrap().tick();
        publish(&app, false);
        thread::sleep(TICK);
    });
}

fn reachable(host: &str, port: u16) -> bool {
    let Ok(mut addrs) = (host, port).to_socket_addrs() else { return false };
    addrs.any(|a| TcpStream::connect_timeout(&a, Duration::from_millis(600)).is_ok())
}

fn spawn_reach(app: AppHandle) {
    thread::spawn(move || loop {
        let targets: Vec<(String, (String, u16))> = {
            let state = app.state::<AppState>();
            let engine = state.engine.lock().unwrap();
            engine.printers.iter().filter(|p| engine.is_tracked(&p.name)).map(|p| (p.name.clone(), p.host.clone())).collect()
        };
        let results: Vec<(String, bool)> = targets
            .into_iter()
            .map(|(name, (host, port))| thread::spawn(move || (name, reachable(&host, port))))
            .filter_map(|h| h.join().ok())
            .collect();
        let mut came_online = Vec::new();
        {
            let state = app.state::<AppState>();
            let mut engine = state.engine.lock().unwrap();
            for (name, ok) in results {
                if ok && engine.reach.get(&name) == Some(&false) && engine.settings.notify {
                    let held = engine.held_count(&name);
                    if held > 0 {
                        came_online.push((name.clone(), held));
                    }
                }
                engine.reach.insert(name, ok);
            }
        }
        for (name, held) in came_online {
            let _ = app.notification().builder().title(name).body(format!("연결됨 · 보류 {held}건")).show();
        }
        publish(&app, false);
        thread::sleep(REACH_EVERY);
    });
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| show_again(app)))
        .plugin(tauri_plugin_notification::init())
        .manage(AppState {
            engine: Mutex::new(Engine::new(Settings::load())),
            ui: Mutex::new(Ui { anchor: None, hidden_at: Instant::now(), last: Snapshot::default() }),
        })
        .invoke_handler(tauri::generate_handler![
            get_state, release, release_all, stop, delete_job, set_managed, set_hold, set_option, fit, hide, quit
        ])
        .setup(|app| {
            // Keep the Run key pointing at this exe if it has moved.
            if app.state::<AppState>().engine.lock().unwrap().settings.autostart {
                settings::set_autostart(true);
            }
            TrayIconBuilder::with_id("main")
                .icon(Image::from_bytes(ICON_IDLE)?)
                .tooltip("Querint")
                .show_menu_on_left_click(false)
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click { button_state: MouseButtonState::Up, rect, .. } = event {
                        toggle(tray.app_handle(), rect);
                    }
                })
                .build(app)?;
            spawn_ticker(app.handle().clone());
            spawn_reach(app.handle().clone());
            Ok(())
        })
        .on_window_event(|window, event| {
            if let (WindowEvent::Focused(false), Some(win)) = (event, window.get_webview_window("main")) {
                hide(win);
                window.state::<AppState>().ui.lock().unwrap().hidden_at = Instant::now();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running Querint");
}
