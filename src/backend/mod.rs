//! Copyright (C) 2026 Gaultier HUBERT
//! SPDX-License-Identifier: GPL-3.0-or-later

//! Platform desktop backends (capture, input, clipboard, monitors, windows, apps).

use hecate_lampad_helper_base::{DesktopInfoResult, MonitorInfo, VirtualDesktop};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use thiserror::Error;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
mod windows_capture;
pub mod shared_keys;

#[derive(Debug, Error)]
pub enum BackendError {
    #[error("no_active_gui_session: {0}")]
    NoSession(String),
    #[error("display_unsupported: {0}")]
    #[allow(dead_code)] // constructed on unsupported OS targets
    Unsupported(String),
    #[error("not_found: {0}")]
    NotFound(String),
    #[error("timeout: {0}")]
    Timeout(String),
    #[error("permission_denied: {0}")]
    #[allow(dead_code)] // constructed on macOS backend
    PermissionDenied(String),
    #[error("{0}")]
    Other(String),
}

pub trait DesktopBackend: Send {
    fn info(&self) -> Result<DesktopInfoResult, BackendError>;
    fn screenshot(&self, params: &Value) -> Result<(Value, Vec<u8>), BackendError>;
    fn move_mouse(&self, params: &Value) -> Result<Value, BackendError>;
    fn click(&self, params: &Value) -> Result<Value, BackendError>;
    fn scroll(&self, params: &Value) -> Result<Value, BackendError>;
    fn drag(&self, params: &Value) -> Result<Value, BackendError>;
    fn type_text(&self, params: &Value) -> Result<Value, BackendError>;
    fn key(&self, params: &Value) -> Result<Value, BackendError>;
    fn clipboard_get(&self, params: &Value) -> Result<(Value, Vec<u8>), BackendError>;
    fn clipboard_set(&self, params: &Value) -> Result<Value, BackendError>;

    fn list_windows(&self, params: &Value) -> Result<Value, BackendError>;
    fn focus_window(&self, params: &Value) -> Result<Value, BackendError>;
    fn launch_app(&self, params: &Value) -> Result<Value, BackendError>;

    /// Run a command in the GUI session. Backends must implement this explicitly
    /// (no default) so policy enforcement is never skipped by accident.
    fn shell_run(&self, params: &Value) -> Result<Value, BackendError>;

    fn wait_window(&self, params: &Value) -> Result<Value, BackendError> {
        let timeout_ms = params
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(15_000);
        let want_focused = params
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("visible")
            == "focused";
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            let listed = self.list_windows(params)?;
            if let Some(window) = first_matching_window(&listed, params) {
                if !want_focused || window.get("focused").and_then(|v| v.as_bool()).unwrap_or(false)
                {
                    return Ok(json!({ "ok": true, "window": window }));
                }
            }
            if Instant::now() >= deadline {
                return Err(BackendError::Timeout(
                    "no matching window before timeout".into(),
                ));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

/// Prompt for macOS TCC permissions when required (no-op on other platforms).
#[cfg(target_os = "macos")]
pub fn request_os_permissions() -> Result<serde_json::Value, BackendError> {
    let report = macos::request_permissions();
    serde_json::to_value(report).map_err(|e| BackendError::Other(e.to_string()))
}

#[cfg(not(target_os = "macos"))]
pub fn request_os_permissions() -> Result<serde_json::Value, BackendError> {
    Ok(serde_json::json!({
        "supported": false,
        "message": "permission prompts are only implemented on macOS",
    }))
}

pub fn create_backend() -> Result<Box<dyn DesktopBackend>, BackendError> {
    #[cfg(target_os = "linux")]
    {
        return Ok(Box::new(linux::LinuxBackend::open()?));
    }
    #[cfg(target_os = "windows")]
    {
        return Ok(Box::new(windows::WindowsBackend::open()?));
    }
    #[cfg(target_os = "macos")]
    {
        return Ok(Box::new(macos::MacosBackend::open()?));
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        Err(BackendError::Unsupported(
            "desktop helper unsupported on this OS".into(),
        ))
    }
}

pub(crate) fn virtual_from_monitors(monitors: &[MonitorInfo]) -> VirtualDesktop {
    if monitors.is_empty() {
        return VirtualDesktop {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
        };
    }
    let min_x = monitors.iter().map(|m| m.x).min().unwrap_or(0);
    let min_y = monitors.iter().map(|m| m.y).min().unwrap_or(0);
    let max_x = monitors
        .iter()
        .map(|m| m.x + m.width as i32)
        .max()
        .unwrap_or(0);
    let max_y = monitors
        .iter()
        .map(|m| m.y + m.height as i32)
        .max()
        .unwrap_or(0);
    VirtualDesktop {
        x: min_x,
        y: min_y,
        width: (max_x - min_x).max(0) as u32,
        height: (max_y - min_y).max(0) as u32,
    }
}

pub(crate) fn session_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".into())
}

pub(crate) fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, BackendError> {
    use image::{ImageBuffer, RgbaImage};
    let img: RgbaImage = ImageBuffer::from_raw(width, height, rgba.to_vec()).ok_or_else(|| {
        BackendError::Other("failed to build image buffer from capture".into())
    })?;
    let mut cursor = std::io::Cursor::new(Vec::new());
    img.write_to(&mut cursor, image::ImageFormat::Png)
        .map_err(|e| BackendError::Other(e.to_string()))?;
    Ok(cursor.into_inner())
}

pub(crate) fn encode_jpeg(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, BackendError> {
    use image::{ImageBuffer, RgbaImage};
    let img: RgbaImage = ImageBuffer::from_raw(width, height, rgba.to_vec()).ok_or_else(|| {
        BackendError::Other("failed to build image buffer from capture".into())
    })?;
    let mut cursor = std::io::Cursor::new(Vec::new());
    img.write_to(&mut cursor, image::ImageFormat::Jpeg)
        .map_err(|e| BackendError::Other(e.to_string()))?;
    Ok(cursor.into_inner())
}

pub(crate) fn crop_rgba(
    width: u32,
    height: u32,
    rgba: &[u8],
    region: Option<(i32, i32, u32, u32)>,
) -> Result<(u32, u32, Vec<u8>), BackendError> {
    let Some((rx, ry, rw, rh)) = region else {
        return Ok((width, height, rgba.to_vec()));
    };
    if rw == 0 || rh == 0 {
        return Err(BackendError::Other("region size must be positive".into()));
    }
    let x0 = rx.max(0) as u32;
    let y0 = ry.max(0) as u32;
    if x0 >= width || y0 >= height {
        return Err(BackendError::Other("region outside capture".into()));
    }
    let x1 = (x0 + rw).min(width);
    let y1 = (y0 + rh).min(height);
    let out_w = x1 - x0;
    let out_h = y1 - y0;
    let mut out = Vec::with_capacity((out_w * out_h * 4) as usize);
    for y in y0..y1 {
        let start = ((y * width + x0) * 4) as usize;
        let end = start + (out_w * 4) as usize;
        out.extend_from_slice(&rgba[start..end]);
    }
    Ok((out_w, out_h, out))
}

pub(crate) fn parse_region(params: &Value) -> Option<(i32, i32, u32, u32)> {
    let region = params.get("region")?;
    Some((
        region.get("x")?.as_f64()? as i32,
        region.get("y")?.as_f64()? as i32,
        region.get("width")?.as_f64()? as u32,
        region.get("height")?.as_f64()? as u32,
    ))
}

pub(crate) fn window_match_needle(params: &Value) -> Option<(String, String)> {
    for key in ["id", "title", "app"] {
        if let Some(value) = params
            .get(key)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            return Some((key.to_string(), value.to_string()));
        }
    }
    None
}

pub(crate) fn window_matches(window: &Value, params: &Value) -> bool {
    let Some((key, needle)) = window_match_needle(params) else {
        return false;
    };
    let Some(hay) = window.get(&key).and_then(|v| v.as_str()) else {
        return false;
    };
    if key == "id" {
        return hay == needle;
    }
    hay.to_lowercase().contains(&needle.to_lowercase())
}

pub(crate) fn first_matching_window(listed: &Value, params: &Value) -> Option<Value> {
    let windows = listed.get("windows")?.as_array()?;
    windows
        .iter()
        .find(|window| window_matches(window, params))
        .cloned()
}

pub(crate) fn run_shell_in_session(params: &Value) -> Result<Value, BackendError> {
    let argv_values = params
        .get("argv")
        .and_then(|v| v.as_array())
        .ok_or_else(|| BackendError::Other("argv required".into()))?;
    if argv_values.is_empty() {
        return Err(BackendError::Other("argv must not be empty".into()));
    }
    let argv: Vec<String> = argv_values
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_string)
                .ok_or_else(|| BackendError::Other("argv entries must be strings".into()))
        })
        .collect::<Result<_, _>>()?;
    let program = argv[0].clone();
    let args: Vec<&str> = argv.iter().skip(1).map(String::as_str).collect();
    let cwd = params.get("cwd").and_then(|v| v.as_str());
    let mut env_map = HashMap::new();
    if let Some(env) = params.get("env").and_then(|v| v.as_object()) {
        for (key, value) in env {
            if let Some(v) = value.as_str() {
                env_map.insert(key.clone(), v.to_string());
            }
        }
    }
    crate::helper_policy::validate_shell_params(&argv, cwd, &env_map).map_err(|error| {
        BackendError::PermissionDenied(error.to_string())
    })?;

    let timeout_secs = params
        .get("timeout_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(30)
        .clamp(1, 3600);

    let mut command = Command::new(&program);
    command.args(&args);
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    for (key, value) in &env_map {
        command.env(key, value);
    }

    let mut child = command
        .spawn()
        .map_err(|e| BackendError::Other(format!("spawn failed: {e}")))?;
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let stdout_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut out) = stdout_pipe {
            let _ = out.read_to_end(&mut buf);
        }
        buf
    });
    let stderr_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut err) = stderr_pipe {
            let _ = err.read_to_end(&mut buf);
        }
        buf
    });

    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(BackendError::Timeout(format!(
                        "process exceeded timeout_secs={timeout_secs}"
                    )));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(BackendError::Other(e.to_string())),
        }
    };

    let stdout = stdout_handle.join().unwrap_or_default();
    let stderr = stderr_handle.join().unwrap_or_default();
    Ok(json!({
        "stdout": String::from_utf8_lossy(&stdout),
        "stderr": String::from_utf8_lossy(&stderr),
        "exit_code": status.code().unwrap_or(-1),
    }))
}

/// After launching an app, optionally wait for a related window.
pub(crate) fn launch_result_with_optional_wait<F>(
    backend: &dyn DesktopBackend,
    params: &Value,
    launched: Value,
    mut related: F,
) -> Result<Value, BackendError>
where
    F: FnMut(&Value) -> bool,
{
    let wait_ms = params
        .get("wait_window_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if wait_ms == 0 {
        return Ok(json!({
            "launched": true,
            "window": null,
            "detail": launched,
        }));
    }
    let deadline = Instant::now() + Duration::from_millis(wait_ms);
    let list_params = json!({ "include_hidden": false });
    while Instant::now() < deadline {
        if let Ok(listed) = backend.list_windows(&list_params) {
            if let Some(windows) = listed.get("windows").and_then(|v| v.as_array()) {
                if let Some(window) = windows.iter().find(|w| related(w)) {
                    return Ok(json!({
                        "launched": true,
                        "window": window,
                        "detail": launched,
                    }));
                }
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(json!({
        "launched": true,
        "window": null,
        "detail": launched,
    }))
}
