//! Computer use on Windows, straight through user32 and gdi32: no helper program to install, and nothing that runs between
//! the calls. The events themselves are worked out in desktop.rs (`Ev`), where they are tested on every system.
use super::{png_rgb, Ev};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::HiDpi::{
    SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SetCursorPos, SM_CXSCREEN, SM_CYSCREEN,
};

/// Pixels as the screen has them, not as a scaled display shows them to an old program: the picture, the click and the
/// screen size then count alike. Only for this thread and only for this call.
struct RealPixels(windows_sys::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT);

impl RealPixels {
    fn new() -> Self {
        Self(unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) })
    }
}

impl Drop for RealPixels {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { SetThreadDpiAwarenessContext(self.0) };
        }
    }
}

fn input(e: &Ev) -> INPUT {
    let mut i: INPUT = unsafe { std::mem::zeroed() };
    match *e {
        Ev::Key { vk, up, ext } => {
            i.r#type = INPUT_KEYBOARD;
            i.Anonymous.ki = KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if up { KEYEVENTF_KEYUP } else { 0 }
                    | if ext { KEYEVENTF_EXTENDEDKEY } else { 0 },
                time: 0,
                dwExtraInfo: 0,
            };
        }
        Ev::Unicode { unit, up } => {
            i.r#type = INPUT_KEYBOARD;
            i.Anonymous.ki = KEYBDINPUT {
                wVk: 0,
                wScan: unit,
                dwFlags: KEYEVENTF_UNICODE | if up { KEYEVENTF_KEYUP } else { 0 },
                time: 0,
                dwExtraInfo: 0,
            };
        }
        Ev::Mouse { flags, data } => {
            i.r#type = INPUT_MOUSE;
            i.Anonymous.mi = MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: data as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            };
        }
    }
    i
}

pub fn send(events: &[Ev]) -> Result<(), String> {
    if events.is_empty() {
        return Ok(());
    }
    let inputs: Vec<INPUT> = events.iter().map(input).collect();
    let sent = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        )
    };
    if sent as usize != inputs.len() {
        // a window of a program that runs as administrator takes no input from one that does not
        return Err(format!(
            "Windows took {sent} of {} inputs ({}). A window that runs as administrator does not take input from FLE Link.",
            inputs.len(),
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

pub fn move_to(x: i64, y: i64) -> Result<(), String> {
    let _px = RealPixels::new();
    let (x, y) = (x.clamp(-32000, 32000) as i32, y.clamp(-32000, 32000) as i32);
    if unsafe { SetCursorPos(x, y) } == 0 {
        return Err(format!(
            "The mouse could not be moved: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// The main screen as PNG, `scale` of its real size.
pub fn screenshot(scale: f64) -> Result<Vec<u8>, String> {
    let _px = RealPixels::new();
    unsafe {
        let (w, h) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
        if w <= 0 || h <= 0 {
            return Err("Windows names no screen.".into());
        }
        let sw = ((w as f64 * scale).round() as i32).max(1);
        let sh = ((h as f64 * scale).round() as i32).max(1);
        let screen = GetDC(std::ptr::null_mut());
        if screen.is_null() {
            return Err("The screen cannot be read (no desktop in this session?).".into());
        }
        let mem = CreateCompatibleDC(screen);
        let bmp = CreateCompatibleBitmap(screen, sw, sh);
        let old = SelectObject(mem, bmp);
        SetStretchBltMode(mem, HALFTONE);
        SetBrushOrgEx(mem, 0, 0, std::ptr::null_mut());
        let copied = StretchBlt(mem, 0, 0, sw, sh, screen, 0, 0, w, h, SRCCOPY | CAPTUREBLT);
        // GetDIBits wants the bitmap out of every device context
        SelectObject(mem, old);
        let mut bi: BITMAPINFO = std::mem::zeroed();
        bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bi.bmiHeader.biWidth = sw;
        bi.bmiHeader.biHeight = -sh; // top row first
        bi.bmiHeader.biPlanes = 1;
        bi.bmiHeader.biBitCount = 32;
        bi.bmiHeader.biCompression = BI_RGB;
        let mut bgra = vec![0u8; sw as usize * sh as usize * 4];
        let lines = if copied != 0 {
            GetDIBits(
                mem,
                bmp,
                0,
                sh as u32,
                bgra.as_mut_ptr() as *mut _,
                &mut bi,
                DIB_RGB_COLORS,
            )
        } else {
            0
        };
        DeleteObject(bmp);
        DeleteDC(mem);
        ReleaseDC(std::ptr::null_mut(), screen);
        if lines != sh {
            return Err(format!(
                "The screen could not be read: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(png_rgb(sw as u32, sh as u32, &bgra, true))
    }
}
