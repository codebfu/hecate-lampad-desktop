//! Copyright (C) 2026 Gaultier HUBERT
//! SPDX-License-Identifier: GPL-3.0-or-later

//! Windows desktop backend.

use super::{
    crop_rgba, encode_jpeg, encode_png, first_matching_window, launch_result_with_optional_wait,
    parse_region, session_user, virtual_from_monitors, windows_capture, BackendError,
    DesktopBackend,
};
use enigo::{Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use hecate_lampad_helper_base::{DesktopInfoResult, MonitorInfo};
const HELPER_VERSION: &str = env!("CARGO_PKG_VERSION");
use serde_json::{json, Value};
use std::sync::{Mutex, Once};
use std::time::Duration;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, BOOL, HWND, LPARAM, TRUE};
use windows::Win32::System::Threading::{
    CreateProcessW, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, STARTUPINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetSystemMetrics, GetWindowTextW, GetWindowThreadProcessId,
    IsWindowVisible, SetForegroundWindow, ShowWindow, SM_CXSCREEN, SM_CYSCREEN, SW_RESTORE,
};

pub struct WindowsBackend;

impl WindowsBackend {
    pub fn open() -> Result<Self, BackendError> {
        ensure_dpi_awareness();
        Ok(Self)
    }

    fn monitors() -> Vec<MonitorInfo> {
        ensure_dpi_awareness();
        let width = unsafe { GetSystemMetrics(SM_CXSCREEN) } as u32;
        let height = unsafe { GetSystemMetrics(SM_CYSCREEN) } as u32;
        vec![MonitorInfo {
            id: 0,
            x: 0,
            y: 0,
            width,
            height,
            scale: 1.0,
            primary: true,
            name: "primary".into(),
        }]
    }

    fn capture() -> Result<(u32, u32, Vec<u8>), BackendError> {
        ensure_dpi_awareness();
        windows_capture::capture_rgba()
    }

    fn enigo() -> Result<Enigo, BackendError> {
        Enigo::new(&Settings::default()).map_err(|e| BackendError::Other(e.to_string()))
    }

    fn map_key(name: &str) -> Result<Key, BackendError> {
        crate::backend::shared_keys::map_key(name)
    }
}

fn ensure_dpi_awareness() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        // Resolve DPI APIs at runtime. Linking SetProcessDpiAwarenessContext
        // (user32, Windows 10+) makes the PE fail to start on Server 2012 R2 /
        // Windows 8.1 with STATUS_ENTRYPOINT_NOT_FOUND (0xC0000139), which
        // surfaces as MSI error 1722 during the install-task custom action.
        unsafe {
            type SetCtx = unsafe extern "system" fn(isize) -> i32;
            type SetAwareness = unsafe extern "system" fn(u32) -> i32;
            type SetDpiAware = unsafe extern "system" fn() -> i32;

            let user32 = windows::Win32::System::LibraryLoader::LoadLibraryW(windows::core::w!(
                "user32.dll"
            ))
            .ok();
            if let Some(module) = user32 {
                if let Some(set_ctx) =
                    windows::Win32::System::LibraryLoader::GetProcAddress(
                        module,
                        windows::core::s!("SetProcessDpiAwarenessContext"),
                    )
                {
                    // DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2 == (HANDLE)-4
                    let set_ctx: SetCtx = std::mem::transmute(set_ctx);
                    if set_ctx(-4) != 0 {
                        return;
                    }
                }
            }

            let shcore = windows::Win32::System::LibraryLoader::LoadLibraryW(windows::core::w!(
                "shcore.dll"
            ))
            .ok();
            if let Some(module) = shcore {
                if let Some(set_awareness) =
                    windows::Win32::System::LibraryLoader::GetProcAddress(
                        module,
                        windows::core::s!("SetProcessDpiAwareness"),
                    )
                {
                    // PROCESS_PER_MONITOR_DPI_AWARE == 2 (Windows 8.1+)
                    let set_awareness: SetAwareness = std::mem::transmute(set_awareness);
                    if set_awareness(2) >= 0 {
                        return;
                    }
                }
            }

            if let Some(module) = user32 {
                if let Some(set_aware) =
                    windows::Win32::System::LibraryLoader::GetProcAddress(
                        module,
                        windows::core::s!("SetProcessDPIAware"),
                    )
                {
                    let set_aware: SetDpiAware = std::mem::transmute(set_aware);
                    let _ = set_aware();
                }
            }
        }
    });
}

impl DesktopBackend for WindowsBackend {
    fn info(&self) -> Result<DesktopInfoResult, BackendError> {
        let monitors = Self::monitors();
        let virtual_desktop = virtual_from_monitors(&monitors);
        Ok(DesktopInfoResult {
            helper_version: HELPER_VERSION.to_string(),
            display_backend: "windows".into(),
            session_user: session_user(),
            clipboard_supported: true,
            monitors,
            virtual_desktop,
            active_sessions: vec![],
        })
    }

    fn screenshot(&self, params: &Value) -> Result<(Value, Vec<u8>), BackendError> {
        let (width, height, rgba) = Self::capture()?;
        let (w, h, cropped) = crop_rgba(width, height, &rgba, parse_region(params))?;
        let format = params
            .get("format")
            .and_then(|v| v.as_str())
            .unwrap_or("png");
        let (bytes, filename, fmt) = if format == "jpeg" {
            (encode_jpeg(w, h, &cropped)?, "screenshot.jpg", "jpeg")
        } else {
            (encode_png(w, h, &cropped)?, "screenshot.png", "png")
        };
        Ok((
            json!({
                "width": w,
                "height": h,
                "format": fmt,
                "display_id": 0,
                "scale": 1.0,
                "filename": filename,
            }),
            bytes,
        ))
    }

    fn move_mouse(&self, params: &Value) -> Result<Value, BackendError> {
        input_move(params)
    }
    fn click(&self, params: &Value) -> Result<Value, BackendError> {
        input_click(params)
    }
    fn scroll(&self, params: &Value) -> Result<Value, BackendError> {
        input_scroll(params)
    }
    fn drag(&self, params: &Value) -> Result<Value, BackendError> {
        input_drag(params)
    }
    fn type_text(&self, params: &Value) -> Result<Value, BackendError> {
        input_type(params)
    }
    fn key(&self, params: &Value) -> Result<Value, BackendError> {
        input_key(params)
    }
    fn clipboard_get(&self, params: &Value) -> Result<(Value, Vec<u8>), BackendError> {
        clipboard_get(params)
    }
    fn clipboard_set(&self, params: &Value) -> Result<Value, BackendError> {
        clipboard_set(params)
    }

    fn list_windows(&self, params: &Value) -> Result<Value, BackendError> {
        let include_hidden = params
            .get("include_hidden")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Ok(json!({ "windows": enumerate_windows(include_hidden) }))
    }

    fn focus_window(&self, params: &Value) -> Result<Value, BackendError> {
        let listed = self.list_windows(&json!({ "include_hidden": true }))?;
        let window = first_matching_window(&listed, params).ok_or_else(|| {
            BackendError::NotFound("no window matched id/title/app".into())
        })?;
        let id = window
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| BackendError::Other("window id missing".into()))?;
        let hwnd = parse_hwnd(id)?;
        unsafe {
            let _ = ShowWindow(hwnd, SW_RESTORE);
            if !SetForegroundWindow(hwnd).as_bool() {
                return Err(BackendError::Other(
                    "SetForegroundWindow failed (focus may be blocked by OS)".into(),
                ));
            }
        }
        Ok(json!({ "ok": true, "window": window }))
    }

    fn launch_app(&self, params: &Value) -> Result<Value, BackendError> {
        let app = params
            .get("app")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| BackendError::Other("app required".into()))?;
        let args: Vec<String> = params
            .get("args")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let cwd = params.get("cwd").and_then(|v| v.as_str());

        // CreateProcessW only, deliberately: ShellExecuteW resolves monikers via the
        // shell (URL handlers, file associations, "App Paths" registry redirection),
        // which lets an "app" string escape the intended executable and bypass
        // allowlist policy checks upstream. CreateProcessW launches exactly the
        // quoted command line with no shell/verb interpretation, and no fallback
        // to a less strict launch path on failure.
        let command_line = build_windows_command_line(app, &args);
        let mut command_line_wide: Vec<u16> =
            command_line.encode_utf16().chain(std::iter::once(0)).collect();
        let cwd_wide: Option<Vec<u16>> =
            cwd.map(|c| c.encode_utf16().chain(std::iter::once(0)).collect());

        let mut startup_info = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            ..Default::default()
        };
        let mut process_info = PROCESS_INFORMATION::default();

        unsafe {
            CreateProcessW(
                PCWSTR::null(),
                PWSTR(command_line_wide.as_mut_ptr()),
                None,
                None,
                false,
                PROCESS_CREATION_FLAGS(0),
                None,
                cwd_wide
                    .as_ref()
                    .map(|v| PCWSTR::from_raw(v.as_ptr()))
                    .unwrap_or(PCWSTR::null()),
                &mut startup_info,
                &mut process_info,
            )
            .map_err(|e| BackendError::NotFound(format!("failed to launch {app}: {e}")))?;
        }
        let pid = process_info.dwProcessId;
        unsafe {
            let _ = CloseHandle(process_info.hProcess);
            let _ = CloseHandle(process_info.hThread);
        }
        let detail = json!({ "method": "CreateProcessW", "pid": pid, "app": app });
        finish_windows_launch(self, params, app, detail)
    }

    fn shell_run(&self, params: &Value) -> Result<Value, BackendError> {
        crate::backend::run_shell_in_session(params)
    }
}

/// Quote a single argument using the same escaping rules as `CommandLineToArgvW`
/// (mirrored by Rust's own `std::process::Command` on Windows), so `CreateProcessW`
/// receives an unambiguous, non-shell-interpreted command line.
fn quote_windows_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_string();
    }
    let mut quoted = String::from("\"");
    let mut chars = arg.chars().peekable();
    loop {
        let mut backslashes = 0;
        while chars.peek() == Some(&'\\') {
            chars.next();
            backslashes += 1;
        }
        match chars.next() {
            Some('"') => {
                quoted.extend(std::iter::repeat('\\').take(backslashes * 2 + 1));
                quoted.push('"');
            }
            Some(c) => {
                quoted.extend(std::iter::repeat('\\').take(backslashes));
                quoted.push(c);
            }
            None => {
                quoted.extend(std::iter::repeat('\\').take(backslashes * 2));
                break;
            }
        }
    }
    quoted.push('"');
    quoted
}

fn build_windows_command_line(app: &str, args: &[String]) -> String {
    let mut parts = Vec::with_capacity(args.len() + 1);
    parts.push(quote_windows_arg(app));
    for arg in args {
        parts.push(quote_windows_arg(arg));
    }
    parts.join(" ")
}

fn finish_windows_launch(
    backend: &WindowsBackend,
    params: &Value,
    app: &str,
    detail: Value,
) -> Result<Value, BackendError> {
    let app_lower = app.to_lowercase();
    launch_result_with_optional_wait(backend, params, detail, |window| {
        let title = window
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();
        let win_app = window
            .get("app")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();
        title.contains(&app_lower) || win_app.contains(&app_lower)
    })
}

struct EnumState {
    include_hidden: bool,
    foreground: isize,
    windows: Vec<Value>,
}

static ENUM_STATE: Mutex<Option<EnumState>> = Mutex::new(None);

unsafe extern "system" fn enum_windows_proc(hwnd: HWND, _lparam: LPARAM) -> BOOL {
    let Ok(mut guard) = ENUM_STATE.lock() else {
        return TRUE;
    };
    let Some(state) = guard.as_mut() else {
        return TRUE;
    };
    let visible = unsafe { IsWindowVisible(hwnd).as_bool() };
    if !state.include_hidden && !visible {
        return TRUE;
    }
    let mut buf = [0u16; 512];
    let len = unsafe { GetWindowTextW(hwnd, &mut buf) };
    let title = if len > 0 {
        String::from_utf16_lossy(&buf[..len as usize])
    } else {
        String::new()
    };
    if title.is_empty() && !state.include_hidden {
        return TRUE;
    }
    let mut pid = 0u32;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    let id = format!("{}", hwnd.0 as isize);
    state.windows.push(json!({
        "id": id,
        "title": title,
        "app": "",
        "pid": pid,
        "focused": hwnd.0 as isize == state.foreground,
        "bounds": null,
    }));
    TRUE
}

fn enumerate_windows(include_hidden: bool) -> Vec<Value> {
    let foreground = unsafe { GetForegroundWindow().0 as isize };
    {
        let mut guard = ENUM_STATE.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some(EnumState {
            include_hidden,
            foreground,
            windows: Vec::new(),
        });
    }
    unsafe {
        let _ = EnumWindows(Some(enum_windows_proc), LPARAM(0));
    }
    let mut guard = ENUM_STATE.lock().unwrap_or_else(|e| e.into_inner());
    guard.take().map(|s| s.windows).unwrap_or_default()
}

fn parse_hwnd(id: &str) -> Result<HWND, BackendError> {
    let value: isize = id
        .parse()
        .map_err(|_| BackendError::Other(format!("invalid window id {id}")))?;
    Ok(HWND(value as *mut core::ffi::c_void))
}

fn input_move(params: &Value) -> Result<Value, BackendError> {
    let x = params.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0) as i32;
    let y = params.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0) as i32;
    let relative = params
        .get("relative")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mut enigo = WindowsBackend::enigo()?;
    let coord = if relative {
        Coordinate::Rel
    } else {
        Coordinate::Abs
    };
    enigo
        .move_mouse(x, y, coord)
        .map_err(|e| BackendError::Other(e.to_string()))?;
    Ok(json!({ "ok": true, "x": x, "y": y, "relative": relative }))
}

fn input_click(params: &Value) -> Result<Value, BackendError> {
    let x = params.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0) as i32;
    let y = params.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0) as i32;
    let count = params.get("count").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
    let button = match params.get("button").and_then(|v| v.as_str()).unwrap_or("left") {
        "right" => Button::Right,
        "middle" => Button::Middle,
        _ => Button::Left,
    };
    let mut enigo = WindowsBackend::enigo()?;
    enigo
        .move_mouse(x, y, Coordinate::Abs)
        .map_err(|e| BackendError::Other(e.to_string()))?;
    for _ in 0..count.max(1) {
        enigo
            .button(button, Direction::Click)
            .map_err(|e| BackendError::Other(e.to_string()))?;
    }
    Ok(json!({ "ok": true, "x": x, "y": y, "count": count }))
}

fn input_scroll(params: &Value) -> Result<Value, BackendError> {
    let x = params.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0) as i32;
    let y = params.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0) as i32;
    let dy = params
        .get("dy")
        .or_else(|| params.get("delta"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0) as i32;
    let dx = params.get("dx").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
    let mut enigo = WindowsBackend::enigo()?;
    enigo
        .move_mouse(x, y, Coordinate::Abs)
        .map_err(|e| BackendError::Other(e.to_string()))?;
    if dy != 0 {
        enigo
            .scroll(dy, enigo::Axis::Vertical)
            .map_err(|e| BackendError::Other(e.to_string()))?;
    }
    if dx != 0 {
        enigo
            .scroll(dx, enigo::Axis::Horizontal)
            .map_err(|e| BackendError::Other(e.to_string()))?;
    }
    Ok(json!({ "ok": true, "dx": dx, "dy": dy }))
}

fn input_drag(params: &Value) -> Result<Value, BackendError> {
    let from = params.get("from").unwrap_or(&Value::Null);
    let to = params.get("to").unwrap_or(&Value::Null);
    let x0 = from.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0) as i32;
    let y0 = from.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0) as i32;
    let x1 = to.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0) as i32;
    let y1 = to.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0) as i32;
    let duration_ms = params
        .get("duration_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(200);
    let button = match params.get("button").and_then(|v| v.as_str()).unwrap_or("left") {
        "right" => Button::Right,
        "middle" => Button::Middle,
        _ => Button::Left,
    };
    let mut enigo = WindowsBackend::enigo()?;
    enigo
        .move_mouse(x0, y0, Coordinate::Abs)
        .map_err(|e| BackendError::Other(e.to_string()))?;
    enigo
        .button(button, Direction::Press)
        .map_err(|e| BackendError::Other(e.to_string()))?;
    let steps = 10u64.max(duration_ms / 20);
    for i in 1..=steps {
        let t = i as f64 / steps as f64;
        let x = x0 + ((x1 - x0) as f64 * t) as i32;
        let y = y0 + ((y1 - y0) as f64 * t) as i32;
        enigo
            .move_mouse(x, y, Coordinate::Abs)
            .map_err(|e| BackendError::Other(e.to_string()))?;
        std::thread::sleep(Duration::from_millis((duration_ms / steps).max(1)));
    }
    enigo
        .button(button, Direction::Release)
        .map_err(|e| BackendError::Other(e.to_string()))?;
    Ok(json!({ "ok": true }))
}

fn input_type(params: &Value) -> Result<Value, BackendError> {
    let text = params
        .get("text")
        .and_then(|v| v.as_str())
        .ok_or_else(|| BackendError::Other("text required".into()))?;
    let mut enigo = WindowsBackend::enigo()?;
    enigo
        .text(text)
        .map_err(|e| BackendError::Other(e.to_string()))?;
    Ok(json!({ "ok": true, "chars": text.chars().count() }))
}

fn input_key(params: &Value) -> Result<Value, BackendError> {
    let key_name = params
        .get("key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| BackendError::Other("key required".into()))?;
    let action = params
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("tap");
    let modifiers = params
        .get("modifiers")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut enigo = WindowsBackend::enigo()?;
    let mut pressed = Vec::new();
    for modifier in &modifiers {
        if let Some(name) = modifier.as_str() {
            let key = WindowsBackend::map_key(name)?;
            enigo
                .key(key, Direction::Press)
                .map_err(|e| BackendError::Other(e.to_string()))?;
            pressed.push(key);
        }
    }
    let key = WindowsBackend::map_key(key_name)?;
    match action {
        "press" => enigo
            .key(key, Direction::Press)
            .map_err(|e| BackendError::Other(e.to_string()))?,
        "release" => enigo
            .key(key, Direction::Release)
            .map_err(|e| BackendError::Other(e.to_string()))?,
        _ => enigo
            .key(key, Direction::Click)
            .map_err(|e| BackendError::Other(e.to_string()))?,
    }
    for key in pressed.into_iter().rev() {
        let _ = enigo.key(key, Direction::Release);
    }
    Ok(json!({ "ok": true, "key": key_name, "action": action }))
}

fn clipboard_get(params: &Value) -> Result<(Value, Vec<u8>), BackendError> {
    let format = params
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("text");
    let mut clipboard =
        arboard::Clipboard::new().map_err(|e| BackendError::Other(e.to_string()))?;
    if format == "image" {
        let image = clipboard
            .get_image()
            .map_err(|e| BackendError::Other(e.to_string()))?;
        let bytes = encode_png(image.width as u32, image.height as u32, &image.bytes)?;
        Ok((
            json!({
                "format": "image",
                "width": image.width,
                "height": image.height,
                "filename": "clipboard.png",
            }),
            bytes,
        ))
    } else {
        let text = clipboard
            .get_text()
            .map_err(|e| BackendError::Other(e.to_string()))?;
        Ok((json!({ "format": "text", "text": text }), Vec::new()))
    }
}

fn clipboard_set(params: &Value) -> Result<Value, BackendError> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|e| BackendError::Other(e.to_string()))?;
    if let Some(text) = params.get("text").and_then(|v| v.as_str()) {
        clipboard
            .set_text(text)
            .map_err(|e| BackendError::Other(e.to_string()))?;
        return Ok(json!({ "ok": true, "format": "text" }));
    }
    if let Some(b64) = params.get("image_base64").and_then(|v| v.as_str()) {
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| BackendError::Other(e.to_string()))?;
        let img = image::load_from_memory(&bytes)
            .map_err(|e| BackendError::Other(e.to_string()))?
            .to_rgba8();
        let (w, h) = img.dimensions();
        clipboard
            .set_image(arboard::ImageData {
                width: w as usize,
                height: h as usize,
                bytes: std::borrow::Cow::Owned(img.into_raw()),
            })
            .map_err(|e| BackendError::Other(e.to_string()))?;
        return Ok(json!({ "ok": true, "format": "image" }));
    }
    Err(BackendError::Other(
        "provide text or image_base64".into(),
    ))
}
