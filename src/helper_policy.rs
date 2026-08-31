//! Copyright (C) 2026 Gaultier HUBERT
//! SPDX-License-Identifier: GPL-3.0-or-later

//! Local helper shell policy loaded from a root-owned config file.

use hecate_lampad_helper_base::policy::{
    check_cwd_policy, check_env_policy, check_shell_policy, ALLOWLIST_WILDCARD, PolicyError,
};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[cfg(unix)]
const DEFAULT_POLICY_PATH_UNIX: &str = "/etc/hecate-lampad/desktop-helper.toml";

#[derive(Debug, Clone, Deserialize)]
pub struct HelperShellPolicy {
    #[serde(default = "wildcard_list")]
    pub allowed_binaries: Vec<String>,
    #[serde(default = "wildcard_list")]
    pub allowed_cwd: Vec<String>,
    #[serde(default = "wildcard_list")]
    pub allowed_env: Vec<String>,
}

fn wildcard_list() -> Vec<String> {
    vec![ALLOWLIST_WILDCARD.to_string()]
}

impl Default for HelperShellPolicy {
    fn default() -> Self {
        Self {
            allowed_binaries: wildcard_list(),
            allowed_cwd: wildcard_list(),
            allowed_env: wildcard_list(),
        }
    }
}

static POLICY: OnceLock<HelperShellPolicy> = OnceLock::new();

pub fn policy_path() -> PathBuf {
    std::env::var_os("HECATE_DESKTOP_HELPER_POLICY")
        .map(PathBuf::from)
        .unwrap_or_else(default_policy_path)
}

fn default_policy_path() -> PathBuf {
    #[cfg(windows)]
    {
        let base = std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".into());
        PathBuf::from(base)
            .join("hecate-lampad")
            .join("desktop-helper.toml")
    }
    #[cfg(not(windows))]
    {
        PathBuf::from(DEFAULT_POLICY_PATH_UNIX)
    }
}

pub fn load_helper_shell_policy() -> HelperShellPolicy {
    POLICY
        .get_or_init(|| load_from_path(&policy_path()))
        .clone()
}

fn policy_file_is_trusted(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let Ok(meta) = std::fs::metadata(path) else {
            return false;
        };
        meta.uid() == 0 && (meta.mode() & 0o022) == 0
    }
    #[cfg(windows)]
    {
        windows_policy_file_is_trusted(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        false
    }
}

#[cfg(windows)]
fn windows_policy_file_is_trusted(path: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::MetadataExt;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::LocalFree;
    use windows::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
    use windows::Win32::Security::{
        IsWellKnownSid, OWNER_SECURITY_INFORMATION, PSID, WinBuiltinAdministratorsSid,
        WinLocalSystemSid,
    };

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if meta.file_type().is_symlink() || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return false;
    }
    let Ok(canonical) = std::fs::canonicalize(path) else {
        return false;
    };
    if !trusted_windows_policy_roots().iter().any(|root| {
        std::fs::canonicalize(root)
            .map(|root| canonical.starts_with(root))
            .unwrap_or(false)
    }) {
        return false;
    }

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut owner = PSID::default();
    let mut sd = windows::Win32::Security::PSECURITY_DESCRIPTOR::default();
    let status = unsafe {
        GetNamedSecurityInfoW(
            PCWSTR(wide.as_ptr()),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            None,
            None,
            &mut sd,
        )
    };
    if status.is_err() {
        return false;
    }
    let trusted = unsafe {
        IsWellKnownSid(owner, WinBuiltinAdministratorsSid).as_bool()
            || IsWellKnownSid(owner, WinLocalSystemSid).as_bool()
    };
    unsafe {
        let _ = LocalFree(windows::Win32::Foundation::HLOCAL(sd.0));
    }
    trusted
}

#[cfg(windows)]
fn trusted_windows_policy_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(program_data) = std::env::var("ProgramData") {
        roots.push(PathBuf::from(program_data).join("hecate-lampad"));
    }
    roots.push(PathBuf::from(r"C:\ProgramData\hecate-lampad"));
    if let Ok(program_files) = std::env::var("ProgramFiles") {
        let program_files = PathBuf::from(program_files);
        roots.push(program_files.join("hecate-lampad-desktop"));
        roots.push(program_files.join("hecate-lampad"));
    }
    roots
}

fn load_from_path(path: &Path) -> HelperShellPolicy {
    if path.exists() && !policy_file_is_trusted(path) {
        tracing::error!(
            path = %path.display(),
            "desktop helper policy is not root-owned or is group/world-writable; using deny-by-default"
        );
        return HelperShellPolicy {
            allowed_binaries: Vec::new(),
            allowed_cwd: Vec::new(),
            allowed_env: Vec::new(),
        };
    }
    match std::fs::read_to_string(path) {
        Ok(raw) => match toml::from_str(&raw) {
            Ok(policy) => policy,
            Err(error) => {
                tracing::warn!(
                    path = %path.display(),
                    %error,
                    "invalid desktop helper policy; using deny-by-default empty allowlists"
                );
                HelperShellPolicy {
                    allowed_binaries: Vec::new(),
                    allowed_cwd: Vec::new(),
                    allowed_env: Vec::new(),
                }
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tracing::warn!(
                path = %path.display(),
                "desktop helper policy missing; using deny-by-default empty allowlists"
            );
            HelperShellPolicy {
                allowed_binaries: Vec::new(),
                allowed_cwd: Vec::new(),
                allowed_env: Vec::new(),
            }
        }
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                %error,
                "failed to read desktop helper policy; using deny-by-default"
            );
            HelperShellPolicy {
                allowed_binaries: Vec::new(),
                allowed_cwd: Vec::new(),
                allowed_env: Vec::new(),
            }
        }
    }
}

pub fn validate_shell_params(
    argv: &[String],
    cwd: Option<&str>,
    env: &HashMap<String, String>,
) -> Result<(), PolicyError> {
    let policy = load_helper_shell_policy();
    check_shell_policy(argv, &policy.allowed_binaries)?;
    if let Some(cwd) = cwd {
        check_cwd_policy(cwd, &policy.allowed_cwd)?;
    }
    check_env_policy(env, &policy.allowed_env)?;
    Ok(())
}
