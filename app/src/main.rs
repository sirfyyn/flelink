//! FLE Link, the desktop app: the same server as `flelink studio`, started inside this program on a free loopback port, and a
//! window that shows it. Closing the window stops the server and any model it started.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Mutex;
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

struct Server(Mutex<Option<flelink_api::run::Embedded>>);

/// Windows has no parent-dies-children-die. A job object with "kill on close" gives it: the app joins one, every process it
/// starts afterwards (the engine) joins too, and Windows ends them all when the app's last handle goes away, also after a crash
/// or "End task". The handle is kept open on purpose until the process ends.
#[cfg(windows)]
fn kill_children_with_me() {
    use windows_sys::Win32::System::JobObjects::*;
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return;
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        AssignProcessToJobObject(job, GetCurrentProcess());
    }
}
#[cfg(not(windows))]
fn kill_children_with_me() {}

fn main() {
    kill_children_with_me();
    let app = tauri::Builder::default()
        // first, so a second start ends before it starts its own server (two agents on one database)
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.show();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let server = flelink_api::run::start_embedded(None, None)?;
            let url = server.url.clone();
            let home: tauri::Url = url.parse()?;
            WebviewWindowBuilder::new(app, "main", WebviewUrl::External(home.clone()))
                .title("FLE Link")
                .inner_size(1280.0, 840.0)
                .min_inner_size(900.0, 600.0)
                // pages of other sites (a sign-in, the docs, GitHub) belong in the user's browser: a link that asks for a
                // new window does nothing in the web view otherwise, and a plain one would replace FLE Link in this window
                .on_new_window(|to, _| {
                    if matches!(to.scheme(), "http" | "https") {
                        flelink_api::run::open_browser(to.as_str());
                    }
                    tauri::webview::NewWindowResponse::Deny
                })
                .on_navigation(move |to| {
                    if to.origin() == home.origin() || !matches!(to.scheme(), "http" | "https") {
                        return true;
                    }
                    flelink_api::run::open_browser(to.as_str());
                    false
                })
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
