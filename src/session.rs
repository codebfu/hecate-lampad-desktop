//! Copyright (C) 2026 Gaultier HUBERT
//! SPDX-License-Identifier: GPL-3.0-or-later

//! In-helper desktop session state (frame buffer + TTL).

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct SessionManager {
    inner: Arc<Mutex<HashMap<String, SessionState>>>,
}

struct SessionState {
    display: Option<u64>,
    fps: u64,
    format: String,
    expires_at: DateTime<Utc>,
    last_frame: Option<(Value, Vec<u8>)>,
    last_capture_at: Option<DateTime<Utc>>,
}

impl Default for SessionManager {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl SessionManager {
    pub fn open(&self, params: &Value) -> Result<Value, String> {
        let session_id = params
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "session_id required".to_string())?
            .to_string();
        let fps = params.get("fps").and_then(|v| v.as_u64()).unwrap_or(2).clamp(1, 10);
        let max_duration = params
            .get("max_duration_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(600)
            .clamp(30, 3600);
        let format = params
            .get("format")
            .and_then(|v| v.as_str())
            .unwrap_or("png")
            .to_string();
        let display = params.get("display").and_then(|v| v.as_u64());
        let mut guard = self.inner.lock().map_err(|_| "session lock poisoned")?;
        guard.insert(
            session_id.clone(),
            SessionState {
                display,
                fps,
                format: format.clone(),
                expires_at: Utc::now() + ChronoDuration::seconds(max_duration as i64),
                last_frame: None,
                last_capture_at: None,
            },
        );
        Ok(serde_json::json!({
            "session_id": session_id,
            "fps": fps,
            "format": format,
            "max_duration_secs": max_duration,
            "display": display,
        }))
    }

    pub fn close(&self, params: &Value) -> Result<Value, String> {
        let session_id = params
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "session_id required".to_string())?;
        let mut guard = self.inner.lock().map_err(|_| "session lock poisoned")?;
        if guard.remove(session_id).is_none() {
            return Err("session not found".into());
        }
        Ok(serde_json::json!({ "session_id": session_id, "closed": true }))
    }

    pub fn active_ids(&self) -> Vec<String> {
        self.inner
            .lock()
            .map(|g| g.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Capture a new frame if FPS interval elapsed (or none yet).
    pub fn frame<F>(
        &self,
        params: &Value,
        mut capture: F,
    ) -> Result<(Value, Vec<u8>), String>
    where
        F: FnMut(&Value) -> Result<(Value, Vec<u8>), String>,
    {
        let session_id = params
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "session_id required".to_string())?
            .to_string();
        let mut guard = self.inner.lock().map_err(|_| "session lock poisoned")?;
        let session = guard
            .get_mut(&session_id)
            .ok_or_else(|| "session not found".to_string())?;
        if Utc::now() >= session.expires_at {
            return Err("session expired".into());
        }

        let interval = ChronoDuration::milliseconds((1000 / session.fps.max(1)) as i64);
        let need_capture = match session.last_capture_at {
            None => true,
            Some(at) => Utc::now() - at >= interval,
        };

        if need_capture {
            let mut shot_params = serde_json::json!({
                "format": session.format,
            });
            if let Some(display) = session.display {
                shot_params
                    .as_object_mut()
                    .unwrap()
                    .insert("display".into(), serde_json::json!(display));
            }
            let (mut meta, bytes) = capture(&shot_params)?;
            if let Some(obj) = meta.as_object_mut() {
                obj.insert("session_id".into(), serde_json::json!(session_id));
                obj.insert(
                    "filename".into(),
                    serde_json::json!(if session.format == "jpeg" {
                        "frame.jpg"
                    } else {
                        "frame.png"
                    }),
                );
            }
            session.last_frame = Some((meta.clone(), bytes.clone()));
            session.last_capture_at = Some(Utc::now());
            Ok((meta, bytes))
        } else if let Some((meta, bytes)) = &session.last_frame {
            Ok((meta.clone(), bytes.clone()))
        } else {
            Err("no frame available".into())
        }
    }
}
