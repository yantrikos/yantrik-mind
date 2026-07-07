//! gphotos — Google Photos connector, honest about a hard 2025 API reality.
//!
//! Google narrowed the Photos **Library API** on 2025-03-31: `mediaItems.list`/`search` now return
//! ONLY items the app itself uploaded — you can no longer read a user's existing library that way.
//! And Google Photos has NEVER exposed face/people grouping via API. So this is NOT an Immich-style
//! people source (Immich stays the moat for that). What Google *does* still offer is the **Picker
//! API**: the user opens a Google-hosted picker on their phone, selects photos, and the app receives
//! download URLs for exactly those items. That is what this connector uses — a pick-based byte pull
//! that brings chosen photos HOME to the box, where the mind's own LOCAL vision captions them.
//! Nothing family-identifying leaves home hardware; Google only sees which photos the user picked.
//!
//! Auth: Google OAuth device-code flow (one phone sign-in; box refreshes forever). Needs a Google
//! Cloud OAuth client of type "TV and Limited Input" → YM_GPHOTOS_CLIENT_ID + YM_GPHOTOS_CLIENT_SECRET
//! (Google's device flow requires the client secret at token exchange even for limited-input clients;
//! it is embeddable, not truly secret). Read-only picker scope — the mind can never write or delete.
//! Token JSON persists at YM_GPHOTOS_TOKEN_PATH (default /var/lib/yantrik-mind/gphotos.json, 0600).

use serde::{Deserialize, Serialize};

const DEVICE: &str = "https://oauth2.googleapis.com/device/code";
const TOKEN: &str = "https://oauth2.googleapis.com/token";
const PICKER: &str = "https://photospicker.googleapis.com/v1";
const SCOPE: &str = "https://www.googleapis.com/auth/photospicker.mediaitems.readonly";

/// One picked photo, normalized. `base_url` is the Google download URL (needs the Bearer header).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpItem {
    pub id: String,
    pub filename: String,
    pub created: String, // YYYY-MM-DD (createTime), best-effort
    pub mime: String,
    pub base_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct GpToken {
    access_token: String,
    refresh_token: String,
    expires_at: i64, // unix seconds
}

/// The device-code prompt handed to the user (they approve on their phone).
#[derive(Debug, Clone)]
pub struct DeviceCode {
    pub user_code: String,
    pub verification_uri: String,
    pub device_code: String,
    pub interval: u64,
    pub expires_in: u64,
}

/// A picker session: the URL the user opens, plus the id the box polls.
#[derive(Debug, Clone)]
pub struct PickSession {
    pub id: String,
    pub picker_uri: String,
    pub poll_interval: u64,
}

pub struct GPhotosClient {
    client_id: String,
    client_secret: String,
    token_path: String,
}

impl GPhotosClient {
    /// Configured only when the OAuth client env is present. Absent → the `gphotos` surface explains
    /// the one-time Google Cloud setup instead of failing silently.
    pub fn from_env() -> Option<GPhotosClient> {
        let client_id = std::env::var("YM_GPHOTOS_CLIENT_ID").ok().filter(|s| !s.trim().is_empty())?;
        let client_secret = std::env::var("YM_GPHOTOS_CLIENT_SECRET").ok().filter(|s| !s.trim().is_empty())?;
        let token_path = std::env::var("YM_GPHOTOS_TOKEN_PATH")
            .unwrap_or_else(|_| "/var/lib/yantrik-mind/gphotos.json".to_string());
        Some(GPhotosClient {
            client_id: client_id.trim().to_string(),
            client_secret: client_secret.trim().to_string(),
            token_path,
        })
    }

    pub fn is_authed(&self) -> bool {
        std::fs::read_to_string(&self.token_path)
            .ok()
            .and_then(|s| serde_json::from_str::<GpToken>(&s).ok())
            .map(|t| !t.refresh_token.is_empty())
            .unwrap_or(false)
    }

    /// Step 1 of device-code auth: the code + URL the user enters on their phone.
    pub async fn begin_auth(&self) -> anyhow::Result<DeviceCode> {
        let client_id = self.client_id.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<DeviceCode> {
            let resp = ureq::post(DEVICE)
                .timeout(std::time::Duration::from_secs(20))
                .send_form(&[("client_id", &client_id), ("scope", SCOPE)]);
            let j: serde_json::Value = match resp {
                Ok(r) => r.into_json()?,
                Err(ureq::Error::Status(_, r)) => {
                    // surface Google's error (e.g. invalid_scope if device flow rejects Picker)
                    let e: serde_json::Value = r.into_json().unwrap_or_default();
                    anyhow::bail!(
                        "Google rejected device-code auth: {} — {}",
                        e["error"].as_str().unwrap_or("error"),
                        e["error_description"].as_str().unwrap_or("(the Picker scope may not be enabled on this OAuth client; enable the Photos Picker API in the Cloud project)")
                    );
                }
                Err(e) => anyhow::bail!("device-code request failed: {e}"),
            };
            Ok(DeviceCode {
                user_code: j["user_code"].as_str().unwrap_or("").to_string(),
                verification_uri: j["verification_url"]
                    .as_str()
                    .or_else(|| j["verification_uri"].as_str())
                    .unwrap_or("https://www.google.com/device")
                    .to_string(),
                device_code: j["device_code"].as_str().unwrap_or("").to_string(),
                interval: j["interval"].as_u64().unwrap_or(5),
                expires_in: j["expires_in"].as_u64().unwrap_or(900),
            })
        })
        .await?
    }

    /// Step 2: poll until the user approves (or timeout). Persists the token on success.
    pub async fn poll_auth(&self, device_code: &str, interval: u64, expires_in: u64, now_secs: i64) -> anyhow::Result<bool> {
        let (client_id, client_secret, token_path, device_code) = (
            self.client_id.clone(),
            self.client_secret.clone(),
            self.token_path.clone(),
            device_code.to_string(),
        );
        tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(expires_in.min(900));
            loop {
                if std::time::Instant::now() >= deadline {
                    return Ok(false);
                }
                std::thread::sleep(std::time::Duration::from_secs(interval.max(5)));
                let resp = ureq::post(TOKEN).timeout(std::time::Duration::from_secs(20)).send_form(&[
                    ("client_id", &client_id),
                    ("client_secret", &client_secret),
                    ("device_code", &device_code),
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ]);
                match resp {
                    Ok(r) => {
                        let j: serde_json::Value = r.into_json()?;
                        let tok = GpToken {
                            access_token: j["access_token"].as_str().unwrap_or("").to_string(),
                            refresh_token: j["refresh_token"].as_str().unwrap_or("").to_string(),
                            expires_at: now_secs + j["expires_in"].as_i64().unwrap_or(3600),
                        };
                        if !tok.access_token.is_empty() {
                            std::fs::write(&token_path, serde_json::to_string(&tok)?)?;
                            #[cfg(unix)]
                            {
                                use std::os::unix::fs::PermissionsExt;
                                let _ = std::fs::set_permissions(&token_path, std::fs::Permissions::from_mode(0o600));
                            }
                            return Ok(true);
                        }
                    }
                    Err(ureq::Error::Status(_, r)) => {
                        let j: serde_json::Value = r.into_json().unwrap_or_default();
                        let err = j["error"].as_str().unwrap_or("");
                        // authorization_pending / slow_down → keep polling; else give up
                        if err != "authorization_pending" && err != "slow_down" {
                            return Ok(false);
                        }
                    }
                    Err(_) => return Ok(false),
                }
            }
        })
        .await?
    }

    /// Fresh access token (refreshing if expired). Blocking-safe.
    fn access(&self, now_secs: i64) -> anyhow::Result<String> {
        let mut tok: GpToken = serde_json::from_str(&std::fs::read_to_string(&self.token_path)?)?;
        if tok.access_token.is_empty() || now_secs >= tok.expires_at - 120 {
            let j: serde_json::Value = ureq::post(TOKEN)
                .timeout(std::time::Duration::from_secs(20))
                .send_form(&[
                    ("client_id", &self.client_id),
                    ("client_secret", &self.client_secret),
                    ("refresh_token", &tok.refresh_token),
                    ("grant_type", "refresh_token"),
                ])?
                .into_json()?;
            tok.access_token = j["access_token"].as_str().unwrap_or("").to_string();
            if let Some(rt) = j["refresh_token"].as_str() {
                tok.refresh_token = rt.to_string();
            }
            tok.expires_at = now_secs + j["expires_in"].as_i64().unwrap_or(3600);
            std::fs::write(&self.token_path, serde_json::to_string(&tok)?)?;
        }
        Ok(tok.access_token)
    }

    /// Create a picker session — returns the URL the user opens on their phone to pick photos.
    pub async fn create_pick_session(&self, now_secs: i64) -> anyhow::Result<PickSession> {
        let this = self.dupe();
        tokio::task::spawn_blocking(move || -> anyhow::Result<PickSession> {
            let token = this.access(now_secs)?;
            let j: serde_json::Value = ureq::post(&format!("{PICKER}/sessions"))
                .set("Authorization", &format!("Bearer {token}"))
                .set("Content-Type", "application/json")
                .timeout(std::time::Duration::from_secs(30))
                .send_string("{}")?
                .into_json()?;
            let poll = j["pollingConfig"]["pollInterval"]
                .as_str()
                .and_then(|s| s.trim_end_matches('s').parse::<f64>().ok())
                .map(|f| f.ceil() as u64)
                .unwrap_or(5);
            Ok(PickSession {
                id: j["id"].as_str().unwrap_or("").to_string(),
                picker_uri: j["pickerUri"].as_str().unwrap_or("").to_string(),
                poll_interval: poll.max(3),
            })
        })
        .await?
    }

    /// Poll a picker session until the user has finished picking (mediaItemsSet) or timeout.
    pub async fn poll_session(&self, session_id: &str, poll_interval: u64, now_secs: i64) -> anyhow::Result<bool> {
        let (this, sid) = (self.dupe(), session_id.to_string());
        tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
            loop {
                if std::time::Instant::now() >= deadline {
                    return Ok(false);
                }
                std::thread::sleep(std::time::Duration::from_secs(poll_interval.max(3)));
                let token = this.access(now_secs)?;
                let j: serde_json::Value = match ureq::get(&format!("{PICKER}/sessions/{sid}"))
                    .set("Authorization", &format!("Bearer {token}"))
                    .timeout(std::time::Duration::from_secs(20))
                    .call()
                {
                    Ok(r) => r.into_json()?,
                    Err(_) => continue,
                };
                if j["mediaItemsSet"].as_bool().unwrap_or(false) {
                    return Ok(true);
                }
            }
        })
        .await?
    }

    /// The photos the user picked in this session (paged, capped).
    pub async fn list_picked(&self, session_id: &str, cap: usize, now_secs: i64) -> anyhow::Result<Vec<GpItem>> {
        let (this, sid) = (self.dupe(), session_id.to_string());
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<GpItem>> {
            let token = this.access(now_secs)?;
            let mut out: Vec<GpItem> = Vec::new();
            let mut page = String::new();
            for _ in 0..10 {
                let mut url = format!("{PICKER}/mediaItems?sessionId={sid}&pageSize=100");
                if !page.is_empty() {
                    url.push_str(&format!("&pageToken={page}"));
                }
                let j: serde_json::Value = ureq::get(&url)
                    .set("Authorization", &format!("Bearer {token}"))
                    .timeout(std::time::Duration::from_secs(30))
                    .call()?
                    .into_json()?;
                for it in j["mediaItems"].as_array().cloned().unwrap_or_default() {
                    let mf = &it["mediaFile"];
                    out.push(GpItem {
                        id: it["id"].as_str().unwrap_or("").to_string(),
                        filename: mf["filename"].as_str().unwrap_or("photo.jpg").to_string(),
                        created: it["createTime"].as_str().map(|d| d.chars().take(10).collect()).unwrap_or_default(),
                        mime: mf["mimeType"].as_str().unwrap_or("image/jpeg").to_string(),
                        base_url: mf["baseUrl"].as_str().unwrap_or("").to_string(),
                    });
                    if out.len() >= cap {
                        return Ok(out);
                    }
                }
                match j["nextPageToken"].as_str() {
                    Some(t) if !t.is_empty() => page = t.to_string(),
                    _ => break,
                }
            }
            Ok(out)
        })
        .await?
    }

    /// Download one picked item's full-resolution bytes (for local vision / import). 20MB cap.
    pub async fn download(&self, base_url: &str, now_secs: i64) -> Option<Vec<u8>> {
        let (this, base_url) = (self.dupe(), base_url.to_string());
        tokio::task::spawn_blocking(move || -> Option<Vec<u8>> {
            let token = this.access(now_secs).ok()?;
            // Picker/Photos baseUrl download parameter: `=d` = full bytes (image download).
            let url = format!("{base_url}=d");
            let resp = ureq::get(&url)
                .set("Authorization", &format!("Bearer {token}"))
                .timeout(std::time::Duration::from_secs(60))
                .call()
                .ok()?;
            let mut buf: Vec<u8> = Vec::new();
            use std::io::Read;
            resp.into_reader().take(20 * 1024 * 1024).read_to_end(&mut buf).ok()?;
            Some(buf)
        })
        .await
        .ok()
        .flatten()
    }

    fn dupe(&self) -> GPhotosClient {
        GPhotosClient {
            client_id: self.client_id.clone(),
            client_secret: self.client_secret.clone(),
            token_path: self.token_path.clone(),
        }
    }
}
