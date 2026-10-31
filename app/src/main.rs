//! FLE Link, the desktop app: the same server as `flelink studio`, started inside this program on a free loopback port, and a
//! window that shows it. Closing the window stops the server and any model it started.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Mutex;
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

struct Server(Mutex<Option<flelink_api::run::Embedded>>);

fn main() {
    let app = tauri::Builder::default()
        .setup(|app| {
            let server = flelink_api::run::start_embedded(None, None)?;
            let url = server.url.clone();
            WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url.parse()?))
                .title("FLE Link")
                .inner_size(1280.0, 840.0)
                .min_inner_size(900.0, 600.0)
                .build()?;
            app.manage(Server(Mutex::new(Some(server))));
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("FLE Link could not start");
    app.run(|handle, event| {
        if let tauri::RunEvent::Exit = event {
            if let Some(s) = handle.try_state::<Server>() {
                if let Some(mut e) = s.0.lock().unwrap().take() {
                    e.stop();
                }
            }
        }
    });
}
