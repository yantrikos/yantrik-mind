//! Cloud photo-source connectors (OneDrive / Google Photos) -- status/auth/pick/find/on-this-day. Extracted from lib.rs.

use super::*;

impl super::ConversationEngine {
    pub async fn onedrive_status(&self) -> String {
        let Some(od) = mind_tools::OneDriveClient::from_env() else {
            return "🗂 OneDrive isn't set up yet. One-time setup:\n1. Register a free Azure app at portal.azure.com → App registrations → New → Accounts in any org + personal Microsoft accounts → set it as a PUBLIC client (Authentication → Allow public client flows: Yes).\n2. Copy the Application (client) ID.\n3. Add YM_OD_CLIENT_ID=<that id> to the box env.\nThen `onedrive auth` and approve on your phone. Read-only (Files.Read) — I can never write or delete.".to_string();
        };
        if od.is_authed() {
            "🗂 OneDrive: connected (read-only). `onedrive recent`, `onedrive find <YYYY-MM-DD..YYYY-MM-DD>`, `onedrive onthisday`.".to_string()
        } else {
            "🗂 OneDrive: app configured but not signed in yet — `onedrive auth` and approve on your phone.".to_string()
        }
    }

    pub async fn onedrive_auth(&self) -> String {
        let Some(od) = mind_tools::OneDriveClient::from_env() else {
            return self.onedrive_status().await;
        };
        if od.is_authed() {
            return "🗂 Already connected. `onedrive recent` to test it.".to_string();
        }
        let dc = match od.begin_auth().await {
            Ok(d) if !d.user_code.is_empty() => d,
            Ok(_) => return "Couldn't start OneDrive sign-in (empty device code) — check YM_OD_CLIENT_ID.".to_string(),
            Err(e) => return format!("OneDrive sign-in failed to start: {e}"),
        };
        // Detached poll — approving on the phone takes a minute; never block the turn.
        let nq = self.notify_queue.clone();
        let now = local_now().timestamp();
        let (code, interval, expires) = (dc.device_code.clone(), dc.interval, dc.expires_in);
        tokio::spawn(async move {
            let Some(od) = mind_tools::OneDriveClient::from_env() else { return };
            match od.poll_auth(&code, interval, expires, now).await {
                Ok(true) => nq.lock().unwrap().push("🗂 OneDrive connected ✅ — I can now reach your older photo years. Try `onedrive onthisday`.".to_string()),
                _ => nq.lock().unwrap().push("🗂 OneDrive sign-in didn't complete (timed out or declined). `onedrive auth` to try again.".to_string()),
            }
        });
        format!(
            "🗂 To connect OneDrive (read-only):\n1. Open {}\n2. Enter code: {}\n3. Sign in and approve.\nI'll confirm here the moment it's done.",
            dc.verification_uri, dc.user_code
        )
    }

    pub async fn gphotos_status(&self) -> String {
        let Some(gp) = mind_tools::GPhotosClient::from_env() else {
            return "📷 Google Photos isn't set up yet.\n\
                 Honest heads-up first: Google narrowed the Photos API in 2025 — an app can no longer \
                 browse your whole library, and Google never exposes WHO is in a photo. So this is NOT \
                 a faces source (Immich already covers that). What it CAN do: you tap-pick photos on \
                 your phone, and I pull just those home and caption them with my own local vision — \
                 Google only sees which ones you picked, nothing family-identifying leaves the house.\n\n\
                 One-time setup:\n\
                 1. In Google Cloud Console (console.cloud.google.com), create/pick a project → enable the \"Photos Picker API\".\n\
                 2. OAuth consent screen → add your own account as a Test user (External, testing is fine).\n\
                 3. Credentials → Create OAuth client ID → application type \"TVs and Limited Input devices\".\n\
                 4. Add to the box env: YM_GPHOTOS_CLIENT_ID=<client id> and YM_GPHOTOS_CLIENT_SECRET=<client secret>.\n\
                 Then `gphotos auth` (approve on your phone) and `gphotos pick`.".to_string();
        };
        if gp.is_authed() {
            "📷 Google Photos: connected (pick-based, read-only). `gphotos pick` — I'll send you a picker link; choose photos and I'll pull + caption them locally.".to_string()
        } else {
            "📷 Google Photos: app configured but not signed in — `gphotos auth` and approve on your phone.".to_string()
        }
    }

    pub async fn gphotos_auth(&self) -> String {
        let Some(gp) = mind_tools::GPhotosClient::from_env() else {
            return self.gphotos_status().await;
        };
        if gp.is_authed() {
            return "📷 Already connected. `gphotos pick` to choose photos.".to_string();
        }
        let dc = match gp.begin_auth().await {
            Ok(d) if !d.user_code.is_empty() => d,
            Ok(_) => return "Couldn't start Google sign-in (empty device code) — check YM_GPHOTOS_CLIENT_ID.".to_string(),
            Err(e) => return format!("Google sign-in failed to start: {e}"),
        };
        let nq = self.notify_queue.clone();
        let now = local_now().timestamp();
        let (code, interval, expires) = (dc.device_code.clone(), dc.interval, dc.expires_in);
        tokio::spawn(async move {
            let Some(gp) = mind_tools::GPhotosClient::from_env() else { return };
            match gp.poll_auth(&code, interval, expires, now).await {
                Ok(true) => nq.lock().unwrap().push("📷 Google Photos connected ✅ — `gphotos pick` and I'll send you a picker link.".to_string()),
                _ => nq.lock().unwrap().push("📷 Google sign-in didn't complete (timed out or declined). `gphotos auth` to try again.".to_string()),
            }
        });
        format!(
            "📷 To connect Google Photos (read-only, pick-based):\n1. Open {}\n2. Enter code: {}\n3. Sign in and approve.\nI'll confirm here the moment it's done.",
            dc.verification_uri, dc.user_code
        )
    }

    /// `gphotos pick` — create a picker session, hand the user the link, then (detached) pull the
    /// photos they pick, caption each with LOCAL vision, and report. Family bytes come home; the
    /// captioning never leaves the box.
    pub async fn gphotos_pick(&self) -> String {
        let Some(gp) = mind_tools::GPhotosClient::from_env() else {
            return self.gphotos_status().await;
        };
        if !gp.is_authed() {
            return "📷 Not connected yet — `gphotos auth` first.".to_string();
        }
        let now = local_now().timestamp();
        let sess = match gp.create_pick_session(now).await {
            Ok(s) if !s.picker_uri.is_empty() => s,
            Ok(_) => return "📷 Couldn't open a picker session (empty picker URL).".to_string(),
            Err(e) => return format!("📷 Couldn't open a picker session: {e}"),
        };
        let nq = self.notify_queue.clone();
        let pq = self.photo_queue.clone();
        let (sid, poll) = (sess.id.clone(), sess.poll_interval);
        tokio::spawn(async move {
            let Some(gp) = mind_tools::GPhotosClient::from_env() else { return };
            let ready = gp.poll_session(&sid, poll, now).await.unwrap_or(false);
            if !ready {
                nq.lock().unwrap().push("📷 No photos picked (the picker timed out). `gphotos pick` to try again.".to_string());
                return;
            }
            let items = gp.list_picked(&sid, 12, now).await.unwrap_or_default();
            if items.is_empty() {
                nq.lock().unwrap().push("📷 The picker closed with nothing selected.".to_string());
                return;
            }
            let vc = mind_tools::VisionClient::from_env();
            let mut captioned = 0usize;
            for it in items.iter().take(6) {
                let Some(bytes) = gp.download(&it.base_url, now).await else { continue };
                let cap = if let Some(v) = &vc {
                    match v
                        .analyze(
                            "In one warm sentence, describe what is happening in this family photo. No preamble.",
                            bytes.clone(),
                            &it.mime,
                        )
                        .await
                    {
                        Ok(t) if !t.trim().is_empty() => format!("📷 {} — {}", it.created, t.trim().chars().take(200).collect::<String>()),
                        _ => format!("📷 {} · {}", it.created, it.filename),
                    }
                } else {
                    format!("📷 {} · {}", it.created, it.filename)
                };
                pq.lock().unwrap().push((bytes, cap, None));
                captioned += 1;
            }
            nq.lock().unwrap().push(format!(
                "📷 Pulled {} picked photo(s) home and captioned {} with local vision. (Immich stays my faces source; this is for photos that live only in Google Photos.)",
                items.len(),
                captioned
            ));
        });
        format!(
            "📷 Open this on your phone and pick the photos you want me to pull home:\n{}\nI'll caption them locally and post them here when you're done.",
            sess.picker_uri
        )
    }

    /// Find OneDrive images in a date window: `find 2019-06-01..2019-06-30` or a single date.
    pub async fn onedrive_find(&self, arg: &str) -> String {
        let Some(od) = mind_tools::OneDriveClient::from_env() else {
            return self.onedrive_status().await;
        };
        if !od.is_authed() {
            return "🗂 Not connected yet — `onedrive auth` first.".to_string();
        }
        let (after, before) = match arg.split_once("..") {
            Some((a, b)) => (a.trim().to_string(), b.trim().to_string()),
            None => {
                let d = arg.trim();
                (d.to_string(), d.to_string())
            }
        };
        if after.len() != 10 || before.len() != 10 {
            return "Usage: onedrive find YYYY-MM-DD..YYYY-MM-DD (or a single YYYY-MM-DD)".to_string();
        }
        let now = local_now().timestamp();
        match od.taken_between(&after, &before, 40, now).await {
            Ok(items) if !items.is_empty() => {
                let by_day: std::collections::BTreeMap<String, usize> = items.iter().fold(Default::default(), |mut m, it| {
                    *m.entry(it.taken.clone()).or_insert(0) += 1;
                    m
                });
                let days: Vec<String> = by_day.iter().rev().take(12).map(|(d, n)| format!("  {d}: {n} photo(s)")).collect();
                format!("🗂 OneDrive — {} image(s) between {after} and {before}:\n{}", items.len(), days.join("\n"))
            }
            Ok(_) => format!("🗂 OneDrive holds no images between {after} and {before}."),
            Err(e) => format!("🗂 OneDrive search failed: {e}"),
        }
    }

    pub async fn onedrive_on_this_day(&self) -> String {
        let Some(od) = mind_tools::OneDriveClient::from_env() else {
            return self.onedrive_status().await;
        };
        if !od.is_authed() {
            return "🗂 Not connected yet — `onedrive auth` first.".to_string();
        }
        let today = local_now().date_naive();
        let mmdd = today.format("%m-%d").to_string();
        let now = local_now().timestamp();
        use chrono::Datelike;
        let mut lines: Vec<String> = Vec::new();
        for y in (2012..today.year()).rev().take(12) {
            let day = format!("{y}-{mmdd}");
            if let Ok(items) = od.taken_between(&day, &day, 5, now).await {
                if !items.is_empty() {
                    lines.push(format!("  {y}: {} photo(s)", items.len()));
                }
            }
        }
        if lines.is_empty() {
            format!("🗂 OneDrive has nothing from {} in past years.", today.format("%B %d"))
        } else {
            format!("🗂 On {} in OneDrive's older years:\n{}", today.format("%B %d"), lines.join("\n"))
        }
    }

}
