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
