//! onedrive — read-only Microsoft Graph client for the family's pre-Immich / unsynced photo years.
//! Device-code auth (one phone sign-in; the box refreshes forever), no inbound surface, no secret
//! (public client). Scope stays Files.Read + offline_access — the mind can read, never write or
//! delete. Token JSON persists at YM_OD_TOKEN_PATH (default /var/lib/yantrik-mind/onedrive.json).

use serde::{Deserialize, Serialize};

const GRAPH: &str = "https://graph.microsoft.com/v1.0";
const AUTH: &str = "https://login.microsoftonline.com/common/oauth2/v2.0";
const SCOPE: &str = "Files.Read offline_access";

/// One OneDrive photo/file item, normalized to the shape the miners want.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OdItem {
    pub id: String,
    pub name: String,
    pub taken: String, // YYYY-MM-DD (photo.takenDateTime or file mtime), best-effort
    pub is_image: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct OdToken {
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

pub struct OneDriveClient {
    client_id: String,
    token_path: String,
}

impl OneDriveClient {
    /// Configured only when a public-client app id is present (YM_OD_CLIENT_ID). Absent → the
    /// `onedrive` surface explains the one-time Azure app registration instead of failing silently.
    pub fn from_env() -> Option<OneDriveClient> {
        let client_id = std::env::var("YM_OD_CLIENT_ID").ok().filter(|s| !s.trim().is_empty())?;
        let token_path = std::env::var("YM_OD_TOKEN_PATH")
            .unwrap_or_else(|_| "/var/lib/yantrik-mind/onedrive.json".to_string());
        Some(OneDriveClient { client_id: client_id.trim().to_string(), token_path })
    }

    pub fn is_authed(&self) -> bool {
        std::fs::read_to_string(&self.token_path)
            .ok()
            .and_then(|s| serde_json::from_str::<OdToken>(&s).ok())
            .map(|t| !t.refresh_token.is_empty())
            .unwrap_or(false)
    }

    /// Step 1 of device-code auth: get the code + URL the user enters on their phone.
    pub async fn begin_auth(&self) -> anyhow::Result<DeviceCode> {
        let client_id = self.client_id.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<DeviceCode> {
            let resp: serde_json::Value = ureq::post(&format!("{AUTH}/devicecode"))
                .timeout(std::time::Duration::from_secs(20))
                .send_form(&[("client_id", &client_id), ("scope", SCOPE)])?
                .into_json()?;
            Ok(DeviceCode {
                user_code: resp["user_code"].as_str().unwrap_or("").to_string(),
                verification_uri: resp["verification_uri"].as_str().unwrap_or("https://microsoft.com/devicelogin").to_string(),
                device_code: resp["device_code"].as_str().unwrap_or("").to_string(),
                interval: resp["interval"].as_u64().unwrap_or(5),
                expires_in: resp["expires_in"].as_u64().unwrap_or(900),
            })
        })
        .await?
    }

    /// Step 2: poll until the user approves (or timeout). Persists the token on success.
    pub async fn poll_auth(&self, device_code: &str, interval: u64, expires_in: u64, now_secs: i64) -> anyhow::Result<bool> {
        let (client_id, token_path, device_code) =
            (self.client_id.clone(), self.token_path.clone(), device_code.to_string());
        tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(expires_in.min(900));
            loop {
                if std::time::Instant::now() >= deadline {
                    return Ok(false);
                }
                std::thread::sleep(std::time::Duration::from_secs(interval.max(3)));
                let resp = ureq::post(&format!("{AUTH}/token"))
                    .timeout(std::time::Duration::from_secs(20))
                    .send_form(&[
                        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                        ("client_id", &client_id),
                        ("device_code", &device_code),
                    ]);
                match resp {
                    Ok(r) => {
                        let j: serde_json::Value = r.into_json()?;
                        let tok = OdToken {
                            access_token: j["access_token"].as_str().unwrap_or("").to_string(),
                            refresh_token: j["refresh_token"].as_str().unwrap_or("").to_string(),
                            expires_at: now_secs + j["expires_in"].as_i64().unwrap_or(3600),
                        };
                        if !tok.access_token.is_empty() {
                            std::fs::write(&token_path, serde_json::to_string(&tok)?)?;
                            // owner-only — the refresh token is a live credential
                            #[cfg(unix)]
                            {
                                use std::os::unix::fs::PermissionsExt;
                                let _ = std::fs::set_permissions(&token_path, std::fs::Permissions::from_mode(0o600));
                            }
                            return Ok(true);
                        }
                    }
                    Err(ureq::Error::Status(400, r)) => {
                        // authorization_pending → keep polling; anything else → stop
                        let j: serde_json::Value = r.into_json().unwrap_or_default();
                        let err = j["error"].as_str().unwrap_or("");
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
        let mut tok: OdToken = serde_json::from_str(&std::fs::read_to_string(&self.token_path)?)?;
        if tok.access_token.is_empty() || now_secs >= tok.expires_at - 120 {
            let j: serde_json::Value = ureq::post(&format!("{AUTH}/token"))
                .timeout(std::time::Duration::from_secs(20))
                .send_form(&[
                    ("grant_type", "refresh_token"),
                    ("client_id", &self.client_id),
                    ("scope", SCOPE),
                    ("refresh_token", &tok.refresh_token),
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

    fn parse_items(v: &serde_json::Value) -> Vec<OdItem> {
        v["value"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|it| {
                let id = it["id"].as_str()?.to_string();
                let name = it["name"].as_str().unwrap_or("").to_string();
                let is_image = it.get("image").is_some()
                    || it["file"]["mimeType"].as_str().map(|m| m.starts_with("image/")).unwrap_or(false);
                let taken = it["photo"]["takenDateTime"]
                    .as_str()
                    .or_else(|| it["fileSystemInfo"]["lastModifiedDateTime"].as_str())
                    .or_else(|| it["lastModifiedDateTime"].as_str())
                    .map(|d| d.chars().take(10).collect::<String>())
                    .unwrap_or_default();
                Some(OdItem { id, name, taken, is_image })
            })
            .collect()
    }

    /// Recent images from the camera-roll view (Graph's special "recent" collection).
    pub async fn recent(&self, n: usize, now_secs: i64) -> anyhow::Result<Vec<OdItem>> {
        let this = self.dupe();
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<OdItem>> {
            let token = this.access(now_secs)?;
            let j: serde_json::Value = ureq::get(&format!("{GRAPH}/me/drive/recent?$top={}", n.clamp(1, 200)))
                .set("Authorization", &format!("Bearer {token}"))
                .timeout(std::time::Duration::from_secs(30))
                .call()?
                .into_json()?;
            Ok(OneDriveClient::parse_items(&j).into_iter().filter(|i| i.is_image).collect())
        })
        .await?
    }

    /// Images whose taken/modified date falls in [after, before] (YYYY-MM-DD). Searches by year
    /// tokens to keep the query cheap, then filters client-side — the Branson/pre-Immich hunt.
    pub async fn taken_between(&self, after: &str, before: &str, n: usize, now_secs: i64) -> anyhow::Result<Vec<OdItem>> {
        let (this, after, before) = (self.dupe(), after.to_string(), before.to_string());
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<OdItem>> {
            let token = this.access(now_secs)?;
            // Graph has no clean date-range filter on personal drives; the reliable path is the
            // Camera Roll children + delta. Query the "Pictures/Camera Roll" tree, page, filter.
            let mut out: Vec<OdItem> = Vec::new();
            let mut url = format!(
                "{GRAPH}/me/drive/root/search(q='')?$top=200&$select=id,name,photo,file,image,fileSystemInfo,lastModifiedDateTime"
            );
            // fall back to a plain recent listing if search misbehaves
            for _page in 0..6 {
                let j: serde_json::Value = match ureq::get(&url)
                    .set("Authorization", &format!("Bearer {token}"))
                    .timeout(std::time::Duration::from_secs(30))
                    .call()
                {
                    Ok(r) => r.into_json()?,
                    Err(_) => break,
                };
                for it in OneDriveClient::parse_items(&j) {
                    if it.is_image && it.taken.len() == 10 && it.taken.as_str() >= after.as_str() && it.taken.as_str() <= before.as_str() {
                        out.push(it);
                    }
                }
                if out.len() >= n {
                    break;
                }
                match j["@odata.nextLink"].as_str() {
                    Some(next) => url = next.to_string(),
                    None => break,
                }
            }
            out.sort_by(|a, b| b.taken.cmp(&a.taken));
            out.truncate(n);
            Ok(out)
        })
        .await?
    }

    /// Download one item's bytes (for vision/thumbnail/import).
    pub async fn download(&self, id: &str, now_secs: i64) -> Option<Vec<u8>> {
        let (this, id) = (self.dupe(), id.to_string());
        tokio::task::spawn_blocking(move || -> Option<Vec<u8>> {
            let token = this.access(now_secs).ok()?;
            let resp = ureq::get(&format!("{GRAPH}/me/drive/items/{id}/content"))
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

    fn dupe(&self) -> OneDriveClient {
        OneDriveClient { client_id: self.client_id.clone(), token_path: self.token_path.clone() }
    }
}
