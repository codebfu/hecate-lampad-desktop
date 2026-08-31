//! Copyright (C) 2026 Gaultier HUBERT
//! SPDX-License-Identifier: GPL-3.0-or-later

//! hecate-lampad-desktop — user-session GUI helper for computer-use commands.

// Avoid a visible console window when Task Scheduler / Explorer starts the helper.
// AttachConsole below still allows CLI output when launched from an existing terminal.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod backend;
mod helper_policy;
mod server;
mod session;

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing::info;

#[derive(Debug, Parser)]
#[command(name = "hecate-lampad-desktop", about = "Hecate desktop session helper", version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Run the helper and serve local IPC for the agent.
    Run {
        /// Override IPC socket / pipe path.
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Print helper version and probe local display backends.
    Info,
    /// Request macOS TCC permissions (Accessibility, Screen Recording, Automation)
    /// when not already granted. Used by the PKG postinstall in the GUI session.
    RequestPermissions,
    /// Register the Windows logon scheduled task (used by the MSI installer).
    #[cfg(windows)]
    InstallTask {
        /// Path to the task XML (defaults to hecate-lampad-desktop-logon.xml next to this exe).
        #[arg(long)]
        xml: Option<PathBuf>,
    },
    /// Remove the Windows logon scheduled task (used by the MSI uninstaller).
    #[cfg(windows)]
    UninstallTask,
}

#[cfg(windows)]
const LOGON_TASK_NAME: &str = "Hecate Lampad Desktop";

/// Reuse the parent terminal's console when present (cmd/PowerShell), so
/// `info` / clap help still print. No-op when started by Task Scheduler.
#[cfg(windows)]
fn attach_parent_console() {
    use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    #[cfg(windows)]
    attach_parent_console();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    match cli.command {
        Commands::Run { socket } => {
            let path = socket.unwrap_or_else(hecate_lampad_helper_base::default_socket_path);
            info!(socket = %path.display(), "starting desktop helper");
            server::run(path).await
        }
        Commands::Info => {
            let backend = backend::create_backend()?;
            let info = backend.info()?;
            println!("{}", serde_json::to_string_pretty(&info)?);
            Ok(())
        }
        Commands::RequestPermissions => {
            let report = backend::request_os_permissions()?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        #[cfg(windows)]
        Commands::InstallTask { xml } => install_windows_logon_task(xml),
        #[cfg(windows)]
        Commands::UninstallTask => uninstall_windows_logon_task(),
    }
}

#[cfg(windows)]
fn install_windows_logon_task(xml: Option<PathBuf>) -> anyhow::Result<()> {
    use std::process::Command;

    let xml_path = match xml {
        Some(path) => path,
        None => {
            let exe = std::env::current_exe()?;
            let dir = exe
                .parent()
                .ok_or_else(|| anyhow::anyhow!("executable has no parent directory"))?;
            dir.join("hecate-lampad-desktop-logon.xml")
        }
    };
    if !xml_path.is_file() {
        anyhow::bail!("logon task XML not found: {}", xml_path.display());
    }

    // Task Scheduler requires a real UTF-16 LE (BOM) document even when the
    // packaged source is UTF-8 for readability in git. Use a unique exclusive
    // temp name so a world-writable TEMP cannot substitute a fixed path.
    let utf16_path = std::env::temp_dir().join(format!(
        "hecate-lampad-desktop-logon-utf16-{}.xml",
        uuid::Uuid::new_v4()
    ));
    let source = std::fs::read(&xml_path)?;
    let utf16_bytes = if source.starts_with(&[0xFF, 0xFE]) || source.starts_with(&[0xFE, 0xFF]) {
        source
    } else {
        let text = String::from_utf8(source)
            .map_err(|e| anyhow::anyhow!("logon task XML is not UTF-8: {e}"))?;
        let mut out = vec![0xFFu8, 0xFE];
        for unit in text.encode_utf16() {
            out.extend_from_slice(&unit.to_le_bytes());
        }
        out
    };
    {
        use std::io::Write;
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&utf16_path)?;
        file.write_all(&utf16_bytes)?;
        file.sync_all()?;
    }

    let create = Command::new("schtasks")
        .args([
            "/Create",
            "/TN",
            LOGON_TASK_NAME,
            "/XML",
            utf16_path
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("non-UTF8 temp XML path"))?,
            "/F",
        ])
        .output()?;
    let _ = std::fs::remove_file(&utf16_path);
    if !create.status.success() {
        let stderr = String::from_utf8_lossy(&create.stderr);
        let stdout = String::from_utf8_lossy(&create.stdout);
        anyhow::bail!(
            "schtasks /Create failed with {}: stdout={} stderr={}",
            create.status,
            stdout.trim(),
            stderr.trim()
        );
    }

    // schtasks /Run of a group-principal task often starts outside an interactive
    // desktop and fails to create the named pipe. Launch into active sessions.
    match start_helper_in_active_sessions() {
        Ok(started) if started > 0 => {
            info!(started, "launched desktop helper into active sessions");
        }
        Ok(_) => {
            info!("no active interactive session; helper will start at next logon");
        }
        Err(error) => {
            tracing::warn!(
                error = %error,
                "failed to launch helper into active sessions; logon task remains registered"
            );
        }
    }

    info!(task = LOGON_TASK_NAME, "registered Windows logon task");
    Ok(())
}

/// Start `hecate-lampad-desktop.exe run` in every active interactive Windows session.
#[cfg(windows)]
fn start_helper_in_active_sessions() -> anyhow::Result<usize> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::RemoteDesktop::{
        WTSEnumerateSessionsW, WTSFreeMemory, WTSQueryUserToken, WTSActive,
        WTS_CURRENT_SERVER_HANDLE, WTS_SESSION_INFOW,
    };
    use windows::Win32::System::Threading::{
        CreateProcessAsUserW, CREATE_UNICODE_ENVIRONMENT, DETACHED_PROCESS, PROCESS_INFORMATION,
        STARTUPINFOW,
    };

    let exe = std::env::current_exe()?;
    let mut command_line: Vec<u16> = Vec::new();
    command_line.push('"' as u16);
    command_line.extend(exe.as_os_str().encode_wide());
    command_line.push('"' as u16);
    command_line.push(' ' as u16);
    command_line.extend("run".encode_utf16());
    command_line.push(0);

    let mut sessions_ptr: *mut WTS_SESSION_INFOW = std::ptr::null_mut();
    let mut count: u32 = 0;
    unsafe {
        WTSEnumerateSessionsW(
            WTS_CURRENT_SERVER_HANDLE,
            0,
            1,
            &mut sessions_ptr,
            &mut count,
        )?;
    }
    if sessions_ptr.is_null() {
        return Ok(0);
    }

    let mut started = 0usize;
    let sessions = unsafe { std::slice::from_raw_parts(sessions_ptr, count as usize) };
    for session in sessions {
        if session.State != WTSActive {
            continue;
        }
        if session.SessionId == 0 {
            // Session 0 is services; skip.
            continue;
        }

        let mut user_token = HANDLE::default();
        let token_result = unsafe { WTSQueryUserToken(session.SessionId, &mut user_token) };
        if token_result.is_err() {
            continue;
        }

        let mut startup = STARTUPINFOW::default();
        startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        let mut process_info = PROCESS_INFORMATION::default();
        let created = unsafe {
            CreateProcessAsUserW(
                user_token,
                None,
                PWSTR(command_line.as_mut_ptr()),
                None,
                None,
                false,
                CREATE_UNICODE_ENVIRONMENT | DETACHED_PROCESS,
                None,
                None,
                &startup,
                &mut process_info,
            )
        };
        unsafe {
            let _ = CloseHandle(user_token);
        }
        if created.is_ok() {
            unsafe {
                let _ = CloseHandle(process_info.hThread);
                let _ = CloseHandle(process_info.hProcess);
            }
            started += 1;
        }
    }

    unsafe {
        WTSFreeMemory(sessions_ptr as *mut _);
    }
    Ok(started)
}

#[cfg(windows)]
fn uninstall_windows_logon_task() -> anyhow::Result<()> {
    use std::process::Command;

    let status = Command::new("schtasks")
        .args(["/Delete", "/TN", LOGON_TASK_NAME, "/F"])
        .status()?;
    if !status.success() {
        // Already absent is fine during uninstall.
        info!(task = LOGON_TASK_NAME, status = %status, "logon task delete returned non-zero");
    }
    Ok(())
}
