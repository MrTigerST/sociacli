#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use tauri::{Manager, PhysicalPosition, WebviewUrl, WebviewWindowBuilder};

const WIDTH: f64 = 380.0;
const HEIGHT: f64 = 460.0;
const MARGIN_X: i32 = 20;
const MARGIN_Y: i32 = 60;

#[tauri::command]
fn set_visible(app: tauri::AppHandle, visible: bool) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = if visible { win.show() } else { win.hide() };
    }
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![set_visible])
        .setup(|app| {
            let win = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                .title("sociacli")
                .inner_size(WIDTH, HEIGHT)
                .resizable(false)
                .decorations(false)
                .always_on_top(true)
                .skip_taskbar(true)
                .transparent(true)
                // Hidden at boot — JS calls `set_visible(true)` on first
                // event and `set_visible(false)` once the last card is gone.
                .visible(false)
                .build()?;

            if let Some(monitor) = win.primary_monitor()? {
                let size = monitor.size();
                let scale = monitor.scale_factor();
                let w = (WIDTH * scale) as i32;
                let h = (HEIGHT * scale) as i32;
                let x = size.width as i32 - w - (MARGIN_X as f64 * scale) as i32;
                let y = size.height as i32 - h - (MARGIN_Y as f64 * scale) as i32;
                win.set_position(PhysicalPosition::new(x, y))?;
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running sociacli overlay");
}
