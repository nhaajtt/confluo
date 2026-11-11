//! The Confluo desktop app: a window on top of the download engine.
//!
//! The window can only call the commands below. It never names a file path to
//! open or delete; it names a download by id and the app looks the path up.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager as _, State};
use confluo_bridge::{Bridge, Offer};
use confluo_engine::manager::{Event, ItemView, Manager, NewDownload, Settings};

struct App {
    manager: Manager,
    token: String,
    bridge: Mutex<Option<Bridge>>,
    /// Whether links sent by the browser start by themselves or wait for a click.
    autostart: Arc<AtomicBool>,
    state_dir: PathBuf,
}

#[derive(Serialize)]
struct InterfaceView {
    name: String,
    label: String,
    description: String,
    medium: String,
    up: bool,
    has_gateway: bool,
    link_bps: u64,
    addrs: Vec<String>,
}

#[derive(Serialize)]
struct RouteView {
    id: String,
    label: String,
    /// False when routing is left to the operating system.
    bound: bool,
}

#[derive(Serialize)]
struct BridgeView {
    port: Option<u16>,
    token: String,
    autostart: bool,
}

#[tauri::command]
fn list(app: State<'_, App>) -> Vec<ItemView> {
    app.manager.list()
}

#[tauri::command]
fn add(app: State<'_, App>, new: NewDownload) -> Result<u64, String> {
    app.manager.add(new)
}

#[tauri::command]
fn pause(app: State<'_, App>, id: u64) {
    app.manager.pause(id);
}

#[tauri::command]
fn resume(app: State<'_, App>, id: u64) {
    app.manager.resume(id);
}

#[tauri::command]
fn remove(app: State<'_, App>, id: u64, delete_file: bool) {
    app.manager.remove(id, delete_file);
}

#[tauri::command]
fn move_to(app: State<'_, App>, id: u64, index: usize) {
    app.manager.move_to(id, index);
}

#[tauri::command]
fn clear_completed(app: State<'_, App>) {
    app.manager.clear_completed();
}

#[tauri::command]
fn get_settings(app: State<'_, App>) -> Settings {
    app.manager.settings()
}

#[tauri::command]
fn set_settings(app: State<'_, App>, settings: Settings) -> Result<(), String> {
    if !settings.download_dir.is_absolute() {
        return Err("the download folder must be a full path".into());
    }
    std::fs::create_dir_all(&settings.download_dir).map_err(|e| format!("cannot use that folder: {e}"))?;
    app.manager.set_settings(settings);
    Ok(())
}

#[tauri::command]
fn interfaces() -> Vec<InterfaceView> {
    confluo_net::list_interfaces()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| i.medium != confluo_net::Medium::Loopback)
        .map(|i| InterfaceView {
            name: i.name,
            label: i.label,
            description: i.description,
            medium: format!("{:?}", i.medium).to_lowercase(),
            up: i.up,
            has_gateway: i.has_gateway,
            link_bps: i.link_bps,
            addrs: i.addrs.iter().map(ToString::to_string).collect(),
        })
        .collect()
}

#[tauri::command]
fn routes(app: State<'_, App>) -> Vec<RouteView> {
    app.manager
        .routes()
        .into_iter()
        .map(|r| RouteView { bound: r.bind.is_some(), id: r.id, label: r.label })
        .collect()
}

#[tauri::command]
fn bridge(app: State<'_, App>) -> BridgeView {
    BridgeView {
        port: app.bridge.lock().unwrap().as_ref().map(|b| b.port),
        token: app.token.clone(),
        autostart: app.autostart.load(Ordering::Relaxed),
    }
}

#[tauri::command]
fn set_bridge_autostart(app: State<'_, App>, autostart: bool) {
    app.autostart.store(autostart, Ordering::Relaxed);
    let _ = std::fs::write(app.state_dir.join("bridge-autostart"), if autostart { "1" } else { "0" });
}

fn finished_path(app: &App, id: u64) -> Option<PathBuf> {
    let item = app.manager.list().into_iter().find(|i| i.id == id)?;
    // The saved list is a file on disk; a path in it is only followed inside the download's own folder.
    item.path.filter(|p| p.parent() == Some(item.dir.as_path()) && p.exists())
}

/// Shows a finished download in the file manager.
#[tauri::command]
fn reveal(app: State<'_, App>, id: u64) -> Result<(), String> {
    let path = finished_path(&app, id).ok_or("the file is not there any more")?;
    show_in_folder(&path).map_err(|e| e.to_string())
}

#[cfg(windows)]
fn show_in_folder(path: &Path) -> std::io::Result<()> {
    // Explorer takes the path as one argument; no shell is involved.
    std::process::Command::new("explorer.exe").arg("/select,").arg(path).spawn().map(|_| ())
}

#[cfg(target_os = "macos")]
fn show_in_folder(path: &Path) -> std::io::Result<()> {
    std::process::Command::new("open").arg("-R").arg(path).spawn().map(|_| ())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn show_in_folder(path: &Path) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(path);
    std::process::Command::new("xdg-open").arg(dir).spawn().map(|_| ())
}

fn offer_to_download(offer: Offer, paused: bool) -> NewDownload {
    let mut headers = Vec::new();
    let mut push = |name: &str, value: Option<String>| {
        // Header values from the browser must not smuggle in extra header lines.
        if let Some(v) = value.filter(|v| !v.is_empty() && !v.contains(['\r', '\n'])) {
            headers.push((name.to_string(), v));
        }
    };
    push("Referer", offer.referrer);
    push("Cookie", offer.cookie);
    push("User-Agent", offer.user_agent);
    NewDownload {
        urls: vec![offer.url],
        file_name: offer.file_name,
        headers,
        paused,
        ..NewDownload::default()
    }
}

fn forward_events(handle: AppHandle, manager: &Manager) {
    let mut events = manager.subscribe();
    tauri::async_runtime::spawn(async move {
        loop {
            match events.recv().await {
                Ok(Event::Item(item)) => {
                    let _ = handle.emit("confluo://item", *item);
                }
                Ok(Event::Removed(id)) => {
                    let _ = handle.emit("confluo://removed", id);
                }
                Ok(Event::Settings(s)) => {
                    let _ = handle.emit("confluo://settings", *s);
                }
                // The window fell behind; it re-reads the whole list.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    let _ = handle.emit("confluo://resync", ());
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // CONFLUO_STATE_DIR keeps test and benchmark runs away from the real list.
            let state_dir = match std::env::var_os("CONFLUO_STATE_DIR").map(PathBuf::from) {
                Some(dir) if dir.is_absolute() => dir,
                _ => app.path().app_data_dir()?,
            };
            let downloads = app.path().download_dir().unwrap_or_else(|_| state_dir.join("downloads"));
            std::fs::create_dir_all(&state_dir)?;
            let token = confluo_bridge::load_or_create_token(&state_dir)?;
            let autostart = Arc::new(AtomicBool::new(
                std::fs::read_to_string(state_dir.join("bridge-autostart")).is_ok_and(|s| s.trim() == "1"),
            ));
            let (manager, bridge) = tauri::async_runtime::block_on(async {
                let manager = Manager::open(&state_dir, &downloads)?;
                let (m, auto) = (manager.clone(), autostart.clone());
                let on_offer = Arc::new(move |offer: Offer| {
                    m.add(offer_to_download(offer, !auto.load(Ordering::Relaxed)))
                });
                // The bridge is optional: without it the app still works, the extension does not.
                let bridge = confluo_bridge::serve(token.clone(), &confluo_bridge::PORTS, on_offer).await.ok();
                Ok::<_, std::io::Error>((manager, bridge))
            })?;
            forward_events(app.handle().clone(), &manager);
            app.manage(App { manager, token, bridge: Mutex::new(bridge), autostart, state_dir });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list,
            add,
            pause,
            resume,
            remove,
            move_to,
            clear_completed,
            get_settings,
            set_settings,
            interfaces,
            routes,
            bridge,
            set_bridge_autostart,
            reveal
        ])
        .run(tauri::generate_context!())
        .expect("failed to start Confluo");
}
