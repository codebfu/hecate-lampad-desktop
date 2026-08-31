//! Copyright (C) 2026 Gaultier HUBERT
//! SPDX-License-Identifier: GPL-3.0-or-later

//! macOS desktop backend (CGDisplay capture + enigo input).

use super::{
    crop_rgba, encode_jpeg, encode_png, first_matching_window, launch_result_with_optional_wait,
    parse_region, session_user, virtual_from_monitors, BackendError, DesktopBackend,
};
use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::CFString;
use core_graphics::display::{CGDisplay, CGPoint};
use enigo::{Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use hecate_lampad_helper_base::{DesktopInfoResult, MonitorInfo};
const HELPER_VERSION: &str = env!("CARGO_PKG_VERSION");
use serde::Serialize;
use serde_json::{json, Value};
use std::process::{Command, Stdio};
use std::time::Duration;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> u8;
    fn AXIsProcessTrustedWithOptions(options: core_foundation::dictionary::CFDictionaryRef) -> u8;
    static kAXTrustedCheckOptionPrompt: core_foundation::string::CFStringRef;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

/// Result of prompting / probing TCC permissions needed by the helper.
#[derive(Debug, Serialize)]
pub struct MacosPermissionReport {
    pub accessibility: PermissionStatus,
    pub screen_recording: PermissionStatus,
    pub automation_system_events: PermissionStatus,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PermissionStatus {
    /// Already granted before this call.
    Granted,
    /// System prompt was shown; still not granted afterward.
    Prompted,
    /// Already denied / unavailable without a prompt path.
    Denied,
}

/// Show native macOS consent dialogs for Accessibility, Screen Recording, and
/// Automation (System Events) when they are not already granted.
///
/// Intended to run in the interactive Aqua session (e.g. PKG postinstall via
/// `launchctl asuser`). Always returns a report; never fails the install.
pub fn request_permissions() -> MacosPermissionReport {
    MacosPermissionReport {
        accessibility: request_accessibility(),
        screen_recording: request_screen_recording(),
        automation_system_events: request_automation_system_events(),
    }
}

fn request_accessibility() -> PermissionStatus {
    unsafe {
        if AXIsProcessTrusted() != 0 {
            return PermissionStatus::Granted;
        }
        let key = CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt);
        let value = CFBoolean::true_value();
        let options: CFDictionary<CFString, CFBoolean> =
            CFDictionary::from_CFType_pairs(&[(key, value)]);
        let _ = AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef());
        if AXIsProcessTrusted() != 0 {
            PermissionStatus::Granted
        } else {
            PermissionStatus::Prompted
        }
    }
}

fn request_screen_recording() -> PermissionStatus {
    unsafe {
        if CGPreflightScreenCaptureAccess() {
            return PermissionStatus::Granted;
        }
        let _ = CGRequestScreenCaptureAccess();
        if CGPreflightScreenCaptureAccess() {
            PermissionStatus::Granted
        } else {
            // macOS often requires a Settings toggle + re-launch after the prompt.
            PermissionStatus::Prompted
        }
    }
}

fn request_automation_system_events() -> PermissionStatus {
    // A minimal System Events query triggers the Automation consent sheet when needed.
    let output = Command::new("osascript")
        .args([
            "-e",
            "tell application \"System Events\" to get name of first process",
        ])
        .output();
    match output {
        Ok(out) if out.status.success() => PermissionStatus::Granted,
        Ok(out) => {
            let err = String::from_utf8_lossy(&out.stderr).to_lowercase();
            if err.contains("not allowed")
                || err.contains("not authorised")
                || err.contains("not authorized")
                || err.contains("(-1743)")
            {
                PermissionStatus::Denied
            } else {
                PermissionStatus::Prompted
            }
        }
        Err(_) => PermissionStatus::Denied,
    }
}

pub struct MacosBackend;

impl MacosBackend {
    pub fn open() -> Result<Self, BackendError> {
        Ok(Self)
    }

    fn monitors() -> Vec<MonitorInfo> {
        let display = CGDisplay::main();
        let bounds = display.bounds();
        vec![MonitorInfo {
            id: 0,
            x: bounds.origin.x as i32,
            y: bounds.origin.y as i32,
            width: bounds.size.width as u32,
            height: bounds.size.height as u32,
            scale: 1.0,
            primary: true,
            name: "main".into(),
        }]
    }

    fn capture() -> Result<(u32, u32, Vec<u8>), BackendError> {
        let display = CGDisplay::main();
        let image = display
            .image()
            .ok_or_else(|| BackendError::Other("CGDisplay image capture failed (Screen Recording permission?)".into()))?;
        let width = image.width() as u32;
        let height = image.height() as u32;
        let data = image.data();
        let bytes = data.bytes();
        // BGRA -> RGBA
        let mut rgba = Vec::with_capacity(bytes.len());
        for chunk in bytes.chunks_exact(4) {
            rgba.extend_from_slice(&[chunk[2], chunk[1], chunk[0], chunk[3]]);
        }
        Ok((width, height, rgba))
    }

    fn enigo() -> Result<Enigo, BackendError> {
        Enigo::new(&Settings::default()).map_err(|e| BackendError::Other(e.to_string()))
    }
}

impl DesktopBackend for MacosBackend {
    fn info(&self) -> Result<DesktopInfoResult, BackendError> {
        let monitors = Self::monitors();
        let virtual_desktop = virtual_from_monitors(&monitors);
        Ok(DesktopInfoResult {
            helper_version: HELPER_VERSION.to_string(),
            display_backend: "macos".into(),
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
        let x = params.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0) as i32;
        let y = params.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0) as i32;
        let relative = params
            .get("relative")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let mut enigo = Self::enigo()?;
        let coord = if relative {
            Coordinate::Rel
        } else {
            Coordinate::Abs
        };
        enigo
            .move_mouse(x, y, coord)
            .map_err(|e| BackendError::Other(e.to_string()))?;
        let _ = CGPoint::new(x as f64, y as f64);
        Ok(json!({ "ok": true, "x": x, "y": y, "relative": relative }))
    }

    fn click(&self, params: &Value) -> Result<Value, BackendError> {
        let x = params.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0) as i32;
        let y = params.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0) as i32;
        let count = params.get("count").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
        let button = match params.get("button").and_then(|v| v.as_str()).unwrap_or("left") {
            "right" => Button::Right,
            "middle" => Button::Middle,
            _ => Button::Left,
        };
        let mut enigo = Self::enigo()?;
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

    fn scroll(&self, params: &Value) -> Result<Value, BackendError> {
        let x = params.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0) as i32;
        let y = params.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0) as i32;
        let dy = params
            .get("dy")
            .or_else(|| params.get("delta"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as i32;
        let dx = params.get("dx").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
        let mut enigo = Self::enigo()?;
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

    fn drag(&self, params: &Value) -> Result<Value, BackendError> {
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
        let button = Button::Left;
        let mut enigo = Self::enigo()?;
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

    fn type_text(&self, params: &Value) -> Result<Value, BackendError> {
        let text = params
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| BackendError::Other("text required".into()))?;
        let mut enigo = Self::enigo()?;
        enigo
            .text(text)
            .map_err(|e| BackendError::Other(e.to_string()))?;
        Ok(json!({ "ok": true, "chars": text.chars().count() }))
    }

    fn key(&self, params: &Value) -> Result<Value, BackendError> {
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
        let mut enigo = Self::enigo()?;
        let mut pressed = Vec::new();
        for modifier in &modifiers {
            if let Some(name) = modifier.as_str() {
                let key = crate::backend::shared_keys::map_key(name)?;
                enigo
                    .key(key, Direction::Press)
                    .map_err(|e| BackendError::Other(e.to_string()))?;
                pressed.push(key);
            }
        }
        let key = crate::backend::shared_keys::map_key(key_name)?;
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

    fn clipboard_get(&self, params: &Value) -> Result<(Value, Vec<u8>), BackendError> {
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

    fn clipboard_set(&self, params: &Value) -> Result<Value, BackendError> {
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

    fn list_windows(&self, params: &Value) -> Result<Value, BackendError> {
        let include_hidden = params
            .get("include_hidden")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Ok(json!({ "windows": macos_list_windows(include_hidden)? }))
    }

    fn focus_window(&self, params: &Value) -> Result<Value, BackendError> {
        let listed = self.list_windows(&json!({ "include_hidden": true }))?;
        let window = first_matching_window(&listed, params).ok_or_else(|| {
            BackendError::NotFound("no window matched id/title/app".into())
        })?;
        let app = window
            .get("app")
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .or_else(|| window.get("title").and_then(|v| v.as_str()))
            .ok_or_else(|| BackendError::Other("window app/title missing".into()))?;
        let script = format!(
            "tell application \"System Events\" to set frontmost of first process whose name is \"{}\" to true",
            app.replace('\\', "\\\\").replace('"', "\\\"")
        );
        let status = Command::new("osascript")
            .args(["-e", &script])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .status()
            .map_err(|e| BackendError::Other(e.to_string()))?;
        if !status.success() {
            // Fallback: open -a activates the app.
            let _ = Command::new("open").args(["-a", app]).status();
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

        let mut cmd = if app.contains('.') && !app.contains('/') {
            // Treat dotted names without path as bundle ids.
            let mut c = Command::new("open");
            c.args(["-b", app]);
            c
        } else if app.starts_with('/') {
            let mut c = Command::new("open");
            c.arg(app);
            c
        } else {
            let mut c = Command::new("open");
            c.args(["-a", app]);
            c
        };
        if !args.is_empty() {
            cmd.arg("--args");
            cmd.args(&args);
        }
        if let Some(cwd) = cwd {
            cmd.current_dir(cwd);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let output = cmd
            .output()
            .map_err(|e| BackendError::NotFound(format!("failed to launch {app}: {e}")))?;
        if !output.status.success() {
            return Err(BackendError::NotFound(format!(
                "open failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        let detail = json!({ "method": "open", "app": app });
        let app_lower = app.to_lowercase();
        launch_result_with_optional_wait(self, params, detail, |window| {
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

    fn shell_run(&self, params: &Value) -> Result<Value, BackendError> {
        crate::backend::run_shell_in_session(params)
    }
}

fn macos_list_windows(include_hidden: bool) -> Result<Vec<Value>, BackendError> {
    // System Events gives process/window titles without linking objc.
    let script = r#"
set out to ""
tell application "System Events"
  repeat with p in (every process whose background only is false)
    set pname to name of p as text
    try
      repeat with w in (every window of p)
        set wtitle to name of w as text
        set out to out & pname & character id 9 & wtitle & linefeed
      end repeat
    end try
  end repeat
end tell
return out
"#;
    let output = Command::new("osascript")
        .args(["-e", script])
        .output()
        .map_err(|e| {
            BackendError::PermissionDenied(format!(
                "osascript window list failed (Accessibility?): {e}"
            ))
        })?;
    if !output.status.success() {
        return Err(BackendError::PermissionDenied(format!(
            "osascript window list failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut windows = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let mut parts = line.splitn(2, '\t');
        let app = parts.next().unwrap_or("").trim();
        let title = parts.next().unwrap_or("").trim();
        if !include_hidden && title.is_empty() {
            continue;
        }
        windows.push(json!({
            "id": format!("macos-{idx}"),
            "title": title,
            "app": app,
            "pid": 0,
            "focused": false,
            "bounds": null,
        }));
    }
    Ok(windows)
}
