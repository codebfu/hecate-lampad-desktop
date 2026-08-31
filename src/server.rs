//! Copyright (C) 2026 Gaultier HUBERT
//! SPDX-License-Identifier: GPL-3.0-or-later

//! IPC server for hecate-lampad-desktop.

use crate::backend::{create_backend, BackendError, DesktopBackend};
use crate::session::SessionManager;
use hecate_lampad_helper_base::{
    auth_token_ok, encode_frame, generate_ipc_token, read_frame, write_ipc_token, IpcErrorBody,
    IpcRequest, IpcResponse,
};
#[cfg(unix)]
use hecate_lampad_helper_base::set_ipc_socket_permissions;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tracing::{info, warn};

pub async fn run(socket_path: PathBuf) -> anyhow::Result<()> {
    let backend = Arc::new(Mutex::new(create_backend()?));
    let sessions = SessionManager::default();
    let auth_token = generate_ipc_token();
    write_ipc_token(&socket_path, &auth_token)?;

    #[cfg(unix)]
    {
        use tokio::net::UnixListener;
        if socket_path.exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let listener = UnixListener::bind(&socket_path)?;
        set_ipc_socket_permissions(&socket_path)?;
        info!(path = %socket_path.display(), "listening for agent IPC");
        spawn_socket_path_watchdog(socket_path.clone());
        loop {
            let (mut stream, _) = listener.accept().await?;
            #[cfg(unix)]
            {
                if let Err(error) = reject_untrusted_peer(&stream) {
                    warn!(%error, "rejected IPC peer");
                    continue;
                }
            }
            let backend = Arc::clone(&backend);
            let sessions = sessions.clone();
            let auth_token = auth_token.clone();
            tokio::spawn(async move {
                if let Err(error) =
                    handle_connection(&mut stream, backend, sessions, &auth_token).await
                {
                    warn!(error = %error, "desktop ipc connection failed");
                }
            });
        }
    }

    #[cfg(windows)]
    {
        use std::ffi::c_void;
        use std::mem::size_of;
        use tokio::net::windows::named_pipe::{PipeMode, ServerOptions};
        use windows::Win32::Security::Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
        };
        use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};

        let pipe_name = socket_path.to_string_lossy().to_string();
        info!(pipe = %pipe_name, "listening for agent IPC");

        // Explicit DACL: LocalSystem (agent service) + Builtin Administrators + Creator Owner
        // (the interactive helper that created the pipe). Do not allow arbitrary Interactive Users.
        let sddl = windows::core::w!("D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;CO)");
        let mut security_descriptor = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl,
                SDDL_REVISION_1,
                &mut security_descriptor,
                None,
            )?;
        }

        let mut first_instance = true;
        loop {
            let mut security_attributes = SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: security_descriptor.0,
                bInheritHandle: false.into(),
            };
            let mut server = unsafe {
                ServerOptions::new()
                    .first_pipe_instance(first_instance)
                    .pipe_mode(PipeMode::Byte)
                    .reject_remote_clients(true)
                    .create_with_security_attributes_raw(
                        &pipe_name,
                        &mut security_attributes as *mut _ as *mut c_void,
                    )?
            };
            first_instance = false;
            server.connect().await?;
            let backend = Arc::clone(&backend);
            let sessions = sessions.clone();
            let auth_token = auth_token.clone();
            tokio::spawn(async move {
                if let Err(error) =
                    handle_connection(&mut server, backend, sessions, &auth_token).await
                {
                    warn!(error = %error, "desktop ipc connection failed");
                }
            });
        }
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = (backend, sessions, socket_path, auth_token);
        anyhow::bail!("desktop helper unsupported on this OS");
    }
}

/// Exit when the bound socket path disappears (e.g. agent RuntimeDirectory was
/// recreated on service restart). systemd user unit Restart= then rebinds a
/// fresh socket the agent can reach again.
#[cfg(unix)]
fn spawn_socket_path_watchdog(socket_path: PathBuf) {
    use std::os::unix::fs::FileTypeExt;
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            let missing = match std::fs::symlink_metadata(&socket_path) {
                Ok(meta) => !meta.file_type().is_socket(),
                Err(_) => true,
            };
            if missing {
                warn!(
                    path = %socket_path.display(),
                    "IPC socket path disappeared; exiting so systemd can restart the helper"
                );
                std::process::exit(1);
            }
        }
    });
}

/// Reject peers that are neither root, the agent service user, nor our own uid.
#[cfg(target_os = "linux")]
fn reject_untrusted_peer(stream: &tokio::net::UnixStream) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = size_of_ucred() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    accept_peer_uid(cred.uid)
}

#[cfg(target_os = "macos")]
fn reject_untrusted_peer(stream: &tokio::net::UnixStream) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    let rc = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    accept_peer_uid(uid)
}

#[cfg(unix)]
fn accept_peer_uid(uid: libc::uid_t) -> std::io::Result<()> {
    let self_uid = unsafe { libc::geteuid() };
    if uid == 0 || uid == self_uid || is_hecate_lampad_uid(uid) {
        return Ok(());
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        format!("peer uid {uid} is not trusted for desktop IPC"),
    ))
}

#[cfg(target_os = "linux")]
fn size_of_ucred() -> usize {
    std::mem::size_of::<libc::ucred>()
}

#[cfg(unix)]
fn is_hecate_lampad_uid(uid: libc::uid_t) -> bool {
    let name = std::ffi::CString::new("hecate-lampad").ok();
    let Some(name) = name else {
        return false;
    };
    let pwd = unsafe { libc::getpwnam(name.as_ptr()) };
    if pwd.is_null() {
        return false;
    }
    unsafe { (*pwd).pw_uid == uid }
}

async fn handle_connection<S>(
    stream: &mut S,
    backend: Arc<Mutex<Box<dyn DesktopBackend>>>,
    sessions: SessionManager,
    expected_token: &str,
) -> anyhow::Result<()>
where
    S: tokio::io::AsyncReadExt + tokio::io::AsyncWriteExt + Unpin,
{
    let (header, _payload) = read_frame(stream).await?;
    let request: IpcRequest = serde_json::from_slice(&header)?;
    if !auth_token_ok(request.auth_token.as_deref(), expected_token) {
        let response = IpcResponse {
            id: request.id.clone(),
            ok: false,
            result: json!({}),
            error: Some(IpcErrorBody {
                code: "unauthorized".into(),
                message: "invalid or missing IPC auth token".into(),
            }),
        };
        let frame = encode_frame(&response, &[])?;
        stream.write_all(&frame).await?;
        return Ok(());
    }
    let (response, payload) = dispatch(&request, &backend, &sessions);
    let frame = encode_frame(&response, &payload)?;
    stream.write_all(&frame).await?;
    Ok(())
}

fn dispatch(
    request: &IpcRequest,
    backend: &Arc<Mutex<Box<dyn DesktopBackend>>>,
    sessions: &SessionManager,
) -> (IpcResponse, Vec<u8>) {
    let result = match request.method.as_str() {
        "ping" => Ok((json!({ "pong": true }), Vec::new())),
        "info" => {
            let guard = match backend.lock() {
                Ok(g) => g,
                Err(_) => {
                    return error_response(&request.id, "remote", "backend lock poisoned");
                }
            };
            match guard.info() {
                Ok(mut info) => {
                    info.active_sessions = sessions.active_ids();
                    match serde_json::to_value(info) {
                        Ok(value) => Ok((value, Vec::new())),
                        Err(e) => Err(("remote".into(), e.to_string())),
                    }
                }
                Err(e) => Err(map_backend_error(e)),
            }
        }
        "screenshot" => call_capture(backend, |b| b.screenshot(&request.params)),
        "move" => call_json(backend, |b| b.move_mouse(&request.params)),
        "click" => call_json(backend, |b| b.click(&request.params)),
        "scroll" => call_json(backend, |b| b.scroll(&request.params)),
        "drag" => call_json(backend, |b| b.drag(&request.params)),
        "type" => call_json(backend, |b| b.type_text(&request.params)),
        "key" => call_json(backend, |b| b.key(&request.params)),
        "clipboard.get" => call_capture(backend, |b| b.clipboard_get(&request.params)),
        "clipboard.set" => call_json(backend, |b| b.clipboard_set(&request.params)),
        "app.launch" => call_json(backend, |b| b.launch_app(&request.params)),
        "window.list" => call_json(backend, |b| b.list_windows(&request.params)),
        "window.focus" => call_json(backend, |b| b.focus_window(&request.params)),
        "window.wait" => call_json(backend, |b| b.wait_window(&request.params)),
        "shell.run" => call_json(backend, |b| b.shell_run(&request.params)),
        "session.open" => match sessions.open(&request.params) {
            Ok(value) => Ok((value, Vec::new())),
            Err(message) => Err(("remote".into(), message)),
        },
        "session.close" => match sessions.close(&request.params) {
            Ok(value) => Ok((value, Vec::new())),
            Err(message) => Err(("remote".into(), message)),
        },
        "session.frame" => {
            let backend = Arc::clone(backend);
            match sessions.frame(&request.params, move |shot_params| {
                let guard = backend
                    .lock()
                    .map_err(|_| "backend lock poisoned".to_string())?;
                guard
                    .screenshot(shot_params)
                    .map_err(|e| e.to_string())
            }) {
                Ok((meta, bytes)) => Ok((meta, bytes)),
                Err(message) => Err(("remote".into(), message)),
            }
        }
        "session.input" => {
            let events = match request.params.get("events").and_then(|v| v.as_array()) {
                Some(events) => events.clone(),
                None => {
                    return error_response(&request.id, "remote", "events required");
                }
            };
            let mut results = Vec::new();
            for event in events {
                let action = event
                    .get("action")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let mut params = event.clone();
                if let Some(obj) = params.as_object_mut() {
                    obj.remove("action");
                }
                let one = match action {
                    "move" => call_json(backend, |b| b.move_mouse(&params)),
                    "click" => call_json(backend, |b| b.click(&params)),
                    "scroll" => call_json(backend, |b| b.scroll(&params)),
                    "drag" => call_json(backend, |b| b.drag(&params)),
                    "type" => call_json(backend, |b| b.type_text(&params)),
                    "key" => call_json(backend, |b| b.key(&params)),
                    other => Err(("remote".into(), format!("unsupported action: {other}"))),
                };
                match one {
                    Ok((value, _)) => results.push(value),
                    Err((code, message)) => {
                        return error_response(&request.id, &code, &message);
                    }
                }
            }
            Ok((json!({ "ok": true, "results": results }), Vec::new()))
        }
        other => Err(("remote".into(), format!("unknown method: {other}"))),
    };

    match result {
        Ok((value, payload)) => (
            IpcResponse {
                id: request.id.clone(),
                ok: true,
                result: value,
                error: None,
            },
            payload,
        ),
        Err((code, message)) => error_response(&request.id, &code, &message),
    }
}

fn call_json<F>(
    backend: &Arc<Mutex<Box<dyn DesktopBackend>>>,
    f: F,
) -> Result<(Value, Vec<u8>), (String, String)>
where
    F: FnOnce(&mut dyn DesktopBackend) -> Result<Value, BackendError>,
{
    let mut guard = backend
        .lock()
        .map_err(|_| ("remote".into(), "backend lock poisoned".into()))?;
    f(guard.as_mut())
        .map(|value| (value, Vec::new()))
        .map_err(map_backend_error)
}

fn call_capture<F>(
    backend: &Arc<Mutex<Box<dyn DesktopBackend>>>,
    f: F,
) -> Result<(Value, Vec<u8>), (String, String)>
where
    F: FnOnce(&mut dyn DesktopBackend) -> Result<(Value, Vec<u8>), BackendError>,
{
    let mut guard = backend
        .lock()
        .map_err(|_| ("remote".into(), "backend lock poisoned".into()))?;
    f(guard.as_mut()).map_err(map_backend_error)
}

fn map_backend_error(error: BackendError) -> (String, String) {
    match error {
        BackendError::NoSession(message) => ("no_active_gui_session".into(), message),
        BackendError::Unsupported(message) => ("display_unsupported".into(), message),
        BackendError::NotFound(message) => ("not_found".into(), message),
        BackendError::Timeout(message) => ("timeout".into(), message),
        BackendError::PermissionDenied(message) => ("permission_denied".into(), message),
        BackendError::Other(message) => ("remote".into(), message),
    }
}

fn error_response(id: &str, code: &str, message: &str) -> (IpcResponse, Vec<u8>) {
    (
        IpcResponse {
            id: id.to_string(),
            ok: false,
            result: Value::Null,
            error: Some(IpcErrorBody {
                code: code.to_string(),
                message: message.to_string(),
            }),
        },
        Vec::new(),
    )
}
