//! Copyright (C) 2026 Gaultier HUBERT
//! SPDX-License-Identifier: GPL-3.0-or-later

//! Linux X11 desktop backend (Wayland returns display_unsupported).

use super::{
    crop_rgba, encode_jpeg, encode_png, first_matching_window, launch_result_with_optional_wait,
    parse_region, session_user, virtual_from_monitors, BackendError, DesktopBackend,
};
use enigo::{Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use hecate_lampad_helper_base::{DesktopInfoResult, MonitorInfo};
const HELPER_VERSION: &str = env!("CARGO_PKG_VERSION");
use serde_json::{json, Value};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ConnectionExt, EventMask, ImageFormat, Screen,
};
use x11rb::rust_connection::RustConnection;

pub struct LinuxBackend {
    display_backend: String,
    conn: RustConnection,
    screen_num: usize,
    /// Keeps X11/Wayland clipboard ownership alive after `clipboard.set`.
    ///
    /// On Linux the clipboard contents are hosted by the owning process; dropping
    /// the last `arboard::Clipboard` clears the selection (unlike Windows/macOS).
    clipboard_owner: Mutex<Option<arboard::Clipboard>>,
}

impl LinuxBackend {
    pub fn open() -> Result<Self, BackendError> {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() && std::env::var_os("DISPLAY").is_none() {
            return Err(BackendError::Unsupported(
                "Wayland-only session is not supported yet; need X11 DISPLAY".into(),
            ));
        }
        let display = std::env::var("DISPLAY").map_err(|_| {
            BackendError::NoSession("DISPLAY is not set (no active X11 session)".into())
        })?;
        let (conn, screen_num) = RustConnection::connect(Some(&display)).map_err(|e| {
            BackendError::NoSession(format!("failed to connect to X11 display {display}: {e}"))
        })?;
        Ok(Self {
            display_backend: "x11".into(),
            conn,
            screen_num,
            clipboard_owner: Mutex::new(None),
        })
    }

    fn owned_clipboard(&self) -> Result<std::sync::MutexGuard<'_, Option<arboard::Clipboard>>, BackendError> {
        let mut guard = self
            .clipboard_owner
            .lock()
            .map_err(|_| BackendError::Other("clipboard lock poisoned".into()))?;
        if guard.is_none() {
            *guard = Some(
                arboard::Clipboard::new().map_err(|e| BackendError::Other(e.to_string()))?,
            );
        }
        Ok(guard)
    }

    fn screen(&self) -> &Screen {
        &self.conn.setup().roots[self.screen_num]
    }

    fn monitors(&self) -> Result<Vec<MonitorInfo>, BackendError> {
        // Minimal single-screen geometry from root window (multi-monitor via Xinerama optional later).
        let screen = self.screen();
        Ok(vec![MonitorInfo {
            id: 0,
            x: 0,
            y: 0,
            width: screen.width_in_pixels as u32,
            height: screen.height_in_pixels as u32,
            scale: 1.0,
            primary: true,
            name: "screen0".into(),
        }])
    }

    fn capture_root(&self) -> Result<(u32, u32, Vec<u8>), BackendError> {
        let screen = self.screen();
        let width = screen.width_in_pixels as u32;
        let height = screen.height_in_pixels as u32;
        let root = screen.root;
        let reply = self
            .conn
            .get_image(ImageFormat::Z_PIXMAP, root, 0, 0, width as u16, height as u16, !0)
            .map_err(|e| BackendError::Other(e.to_string()))?
            .reply()
            .map_err(|e| BackendError::Other(e.to_string()))?;

        let depth = reply.depth;
        let data = reply.data;
        let mut rgba = Vec::with_capacity((width as usize) * (height as usize) * 4);
        match depth {
            24 | 32 => {
                // Each pixel is 4 bytes in ZPixmap for depth 24/32 on little-endian X11.
                if data.len() < (width as usize) * (height as usize) * 4 {
                    return Err(BackendError::Other(format!(
                        "unexpected image buffer size {} for {}x{} depth {depth}",
                        data.len(),
                        width,
                        height
                    )));
                }
                for chunk in data.chunks_exact(4) {
                    // BGRX
                    rgba.extend_from_slice(&[chunk[2], chunk[1], chunk[0], 255]);
                }
            }
            _ => {
                return Err(BackendError::Unsupported(format!(
                    "unsupported X11 depth {depth}"
                )));
            }
        }
        Ok((width, height, rgba))
    }

    fn enigo() -> Result<Enigo, BackendError> {
        Enigo::new(&Settings::default()).map_err(|e| BackendError::Other(e.to_string()))
    }

    fn resolve_xy(&self, params: &Value) -> Result<(i32, i32), BackendError> {
        let x = params
            .get("x")
            .and_then(|v| v.as_f64())
            .ok_or_else(|| BackendError::Other("x required".into()))? as i32;
        let y = params
            .get("y")
            .and_then(|v| v.as_f64())
            .ok_or_else(|| BackendError::Other("y required".into()))? as i32;
        Ok((x, y))
    }

    fn button_from(params: &Value) -> Button {
        match params.get("button").and_then(|v| v.as_str()).unwrap_or("left") {
            "right" => Button::Right,
            "middle" => Button::Middle,
            _ => Button::Left,
        }
    }

    fn map_key(name: &str) -> Result<Key, BackendError> {
        crate::backend::shared_keys::map_key(name)
    }
}

impl DesktopBackend for LinuxBackend {
    fn info(&self) -> Result<DesktopInfoResult, BackendError> {
        let monitors = self.monitors()?;
        let virtual_desktop = virtual_from_monitors(&monitors);
        Ok(DesktopInfoResult {
            helper_version: HELPER_VERSION.to_string(),
            display_backend: self.display_backend.clone(),
            session_user: session_user(),
            clipboard_supported: true,
            monitors,
            virtual_desktop,
            active_sessions: vec![],
        })
    }

    fn screenshot(&self, params: &Value) -> Result<(Value, Vec<u8>), BackendError> {
        let (width, height, rgba) = self.capture_root()?;
        let region = parse_region(params);
        // If display is set and not 0, reject for now (single screen).
        if let Some(display) = params.get("display").and_then(|v| v.as_u64()) {
            if display != 0 {
                return Err(BackendError::Other(
                    "only display 0 is available on this X11 backend".into(),
                ));
            }
        }
        let (w, h, cropped) = crop_rgba(width, height, &rgba, region)?;
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
        let (x, y) = self.resolve_xy(params)?;
        let relative = params
            .get("relative")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let mut enigo = Self::enigo()?;
        if relative {
            enigo
                .move_mouse(x, y, Coordinate::Rel)
                .map_err(|e| BackendError::Other(e.to_string()))?;
        } else {
            enigo
                .move_mouse(x, y, Coordinate::Abs)
                .map_err(|e| BackendError::Other(e.to_string()))?;
        }
        Ok(json!({ "ok": true, "x": x, "y": y, "relative": relative }))
    }

    fn click(&self, params: &Value) -> Result<Value, BackendError> {
        let (x, y) = self.resolve_xy(params)?;
        let count = params.get("count").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
        let button = Self::button_from(params);
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
        let (x, y) = self.resolve_xy(params)?;
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
        Ok(json!({ "ok": true, "x": x, "y": y, "dx": dx, "dy": dy }))
    }

    fn drag(&self, params: &Value) -> Result<Value, BackendError> {
        let from = params
            .get("from")
            .ok_or_else(|| BackendError::Other("from required".into()))?;
        let to = params
            .get("to")
            .ok_or_else(|| BackendError::Other("to required".into()))?;
        let x0 = from.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0) as i32;
        let y0 = from.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0) as i32;
        let x1 = to.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0) as i32;
        let y1 = to.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0) as i32;
        let duration_ms = params
            .get("duration_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(200);
        let button = Self::button_from(params);
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
        Ok(json!({ "ok": true, "from": {"x": x0, "y": y0}, "to": {"x": x1, "y": y1} }))
    }

    fn type_text(&self, params: &Value) -> Result<Value, BackendError> {
        let text = params
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| BackendError::Other("text required".into()))?;
        let delay_ms = params.get("delay_ms").and_then(|v| v.as_u64()).unwrap_or(0);
        let mut enigo = Self::enigo()?;
        if delay_ms == 0 {
            enigo
                .text(text)
                .map_err(|e| BackendError::Other(e.to_string()))?;
        } else {
            for ch in text.chars() {
                enigo
                    .key(Key::Unicode(ch), Direction::Click)
                    .map_err(|e| BackendError::Other(e.to_string()))?;
                std::thread::sleep(Duration::from_millis(delay_ms));
            }
        }
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
                let key = Self::map_key(name)?;
                enigo
                    .key(key, Direction::Press)
                    .map_err(|e| BackendError::Other(e.to_string()))?;
                pressed.push(key);
            }
        }
        let key = Self::map_key(key_name)?;
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
        // Retain the Clipboard handle so X11 selection ownership survives the IPC call.
        let mut owner = self.owned_clipboard()?;
        let Some(clipboard) = owner.as_mut() else {
            return Err(BackendError::Other("clipboard owner missing after init".into()));
        };
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
        let windows = self.enumerate_windows(include_hidden)?;
        Ok(json!({ "windows": windows }))
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
        let xid: u32 = id
            .parse()
            .map_err(|_| BackendError::Other(format!("invalid window id {id}")))?;
        self.focus_xid(xid)?;
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

        let detail = if app.ends_with(".desktop") || !app.contains('/') {
            // Prefer gtk-launch for desktop ids / short names.
            let mut cmd = Command::new("gtk-launch");
            cmd.arg(app.strip_suffix(".desktop").unwrap_or(app));
            for arg in &args {
                cmd.arg(arg);
            }
            if let Some(cwd) = cwd {
                cmd.current_dir(cwd);
            }
            cmd.stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            match cmd.spawn() {
                Ok(child) => json!({ "method": "gtk-launch", "pid": child.id(), "app": app }),
                Err(_) => self.spawn_executable(app, &args, cwd)?,
            }
        } else {
            self.spawn_executable(app, &args, cwd)?
        };

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

impl LinuxBackend {
    fn spawn_executable(
        &self,
        app: &str,
        args: &[String],
        cwd: Option<&str>,
    ) -> Result<Value, BackendError> {
        let mut cmd = Command::new(app);
        cmd.args(args);
        if let Some(cwd) = cwd {
            cmd.current_dir(cwd);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = cmd
            .spawn()
            .map_err(|e| BackendError::NotFound(format!("failed to launch {app}: {e}")))?;
        Ok(json!({ "method": "exec", "pid": child.id(), "app": app }))
    }

    fn intern(&self, name: &str) -> Result<u32, BackendError> {
        let atom = self
            .conn
            .intern_atom(false, name.as_bytes())
            .map_err(|e| BackendError::Other(e.to_string()))?
            .reply()
            .map_err(|e| BackendError::Other(e.to_string()))?
            .atom;
        Ok(atom)
    }

    fn enumerate_windows(&self, include_hidden: bool) -> Result<Vec<Value>, BackendError> {
        let root = self.screen().root;
        let net_client_list = self.intern("_NET_CLIENT_LIST")?;
        let net_wm_name = self.intern("_NET_WM_NAME")?;
        let net_wm_pid = self.intern("_NET_WM_PID")?;
        let net_active = self.intern("_NET_ACTIVE_WINDOW")?;
        let utf8 = self.intern("UTF8_STRING")?;

        let list_reply = self
            .conn
            .get_property(false, root, net_client_list, AtomEnum::WINDOW, 0, 8192)
            .map_err(|e| BackendError::Other(e.to_string()))?
            .reply()
            .map_err(|e| BackendError::Other(e.to_string()))?;
        let window_ids = list_reply
            .value32()
            .ok_or_else(|| BackendError::Other("_NET_CLIENT_LIST missing".into()))?
            .collect::<Vec<_>>();

        let active = self
            .conn
            .get_property(false, root, net_active, AtomEnum::WINDOW, 0, 1)
            .ok()
            .and_then(|c| c.reply().ok())
            .and_then(|r| r.value32().and_then(|mut it| it.next()))
            .unwrap_or(0);

        let mut windows = Vec::new();
        for xid in window_ids {
            let title = self
                .window_string_prop(xid, net_wm_name, utf8)
                .or_else(|| self.window_string_prop(xid, AtomEnum::WM_NAME.into(), AtomEnum::STRING.into()))
                .unwrap_or_default();
            let pid = self
                .conn
                .get_property(false, xid, net_wm_pid, AtomEnum::CARDINAL, 0, 1)
                .ok()
                .and_then(|c| c.reply().ok())
                .and_then(|r| r.value32().and_then(|mut it| it.next()))
                .unwrap_or(0);
            let attrs = self
                .conn
                .get_geometry(xid)
                .ok()
                .and_then(|c| c.reply().ok());
            let (x, y, w, h) = attrs
                .map(|g| (g.x as i32, g.y as i32, g.width as u32, g.height as u32))
                .unwrap_or((0, 0, 0, 0));
            let map_state_viewable = self
                .conn
                .get_window_attributes(xid)
                .ok()
                .and_then(|c| c.reply().ok())
                .map(|a| a.map_state == x11rb::protocol::xproto::MapState::VIEWABLE)
                .unwrap_or(false);
            if !include_hidden && !map_state_viewable {
                continue;
            }
            let app = if pid > 0 {
                std::fs::read_to_string(format!("/proc/{pid}/comm"))
                    .unwrap_or_default()
                    .trim()
                    .to_string()
            } else {
                String::new()
            };
            windows.push(json!({
                "id": xid.to_string(),
                "title": title,
                "app": app,
                "pid": pid,
                "focused": xid == active,
                "bounds": { "x": x, "y": y, "width": w, "height": h },
            }));
        }
        Ok(windows)
    }

    fn window_string_prop(&self, window: u32, atom: u32, typ: u32) -> Option<String> {
        let reply = self
            .conn
            .get_property(false, window, atom, typ, 0, 4096)
            .ok()?
            .reply()
            .ok()?;
        if reply.value.is_empty() {
            return None;
        }
        String::from_utf8(reply.value).ok().map(|s| s.trim_end_matches('\0').to_string())
    }

    fn focus_xid(&self, xid: u32) -> Result<(), BackendError> {
        let root = self.screen().root;
        let net_active = self.intern("_NET_ACTIVE_WINDOW")?;
        let event = ClientMessageEvent::new(32, xid, net_active, [2, 0, 0, 0, 0]);
        self.conn
            .send_event(
                false,
                root,
                EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                event,
            )
            .map_err(|e| BackendError::Other(e.to_string()))?;
        self.conn
            .configure_window(
                xid,
                &x11rb::protocol::xproto::ConfigureWindowAux::new().stack_mode(
                    x11rb::protocol::xproto::StackMode::ABOVE,
                ),
            )
            .map_err(|e| BackendError::Other(e.to_string()))?;
        self.conn
            .set_input_focus(
                x11rb::protocol::xproto::InputFocus::PARENT,
                xid,
                x11rb::CURRENT_TIME,
            )
            .map_err(|e| BackendError::Other(e.to_string()))?;
        self.conn
            .flush()
            .map_err(|e| BackendError::Other(e.to_string()))?;
        Ok(())
    }
}
