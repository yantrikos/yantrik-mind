//! Cost treasury -- daily LLM-spend envelope, draw/report, internal spend ledger. Extracted from lib.rs.

use super::*;

impl super::ConversationEngine {
    /// ---------- PROVIDER QUOTA (ground truth, honestly bounded) ----------
    /// What each provider will actually tell us: NanoGPT has a real balance API; Ollama Cloud and
    /// MiniMax expose nothing programmatic (web dashboards only) — for those we report OUR observed
    /// served/failed counts from the chain layer, which is also how a dry first-hop shows itself.
    pub async fn providers_report(&self) -> String {
        let mut out = String::from("🔌 PROVIDERS — real quota where queryable, observed truth elsewhere\n");
        // Live balance (blocking probe off the async thread).
        let q = tokio::task::spawn_blocking(mind_tools::nanogpt_quota).await.ok().flatten();
        let bal = tokio::task::spawn_blocking(mind_tools::nanogpt_balance).await.ok().flatten();
        let sub_active = q.as_ref().and_then(|v| v.get("active")).and_then(|x| x.as_bool()).unwrap_or(false);
        if sub_active {
            let w = q.as_ref().and_then(|v| v.get("weeklyInputTokens")).cloned().unwrap_or_default();
            let used = w.get("used").and_then(|x| x.as_i64()).unwrap_or(0);
            let rem = w.get("remaining").and_then(|x| x.as_i64()).unwrap_or(0);
            let pct = w.get("percentUsed").and_then(|x| x.as_f64()).unwrap_or(0.0) * 100.0;
            let reset = w
                .get("resetAt")
                .and_then(|x| x.as_i64())
                .and_then(chrono::DateTime::from_timestamp_millis)
                .map(|t| t.with_timezone(local_now().offset()).format("%a %b %-d").to_string())
                .unwrap_or_default();
            out.push_str(&format!(
                "• nanogpt: SUBSCRIPTION active — weekly input tokens {:.1}M/{:.0}M used ({pct:.1}%), {:.1}M remaining, resets {reset}",
                used as f64 / 1e6,
                (used + rem) as f64 / 1e6,
                rem as f64 / 1e6
            ));
            if let Some((usd, _)) = bal {
                out.push_str(&format!(" · PAYG wallet ${usd:.2}"));
            }
            out.push('\n');
        } else {
            match bal {
                Some((usd, _)) => out.push_str(&format!(
                    "• nanogpt: ${usd:.2} PAYG remaining{}\n",
                    if usd < 0.50 { " ⚠️ DRY — calls fail over to the next provider" } else { "" }
                )),
                None => out.push_str("• nanogpt: quota probe failed (key missing or endpoint down)\n"),
            }
        }
        out.push_str("• ollama-cloud: no usage API (dashboard: ollama.com/settings) — observed counts below\n");
        out.push_str("• minimax: no usage API (dashboard: platform.minimax.io) — observed counts below\n");
        let a = tokio::task::spawn_blocking(mind_tools::anthropic_subscription_usage).await.ok().flatten();
        match a {
            Some(v) if v.get("five_hour").is_some() => {
                let pct = |k: &str| v.get(k).and_then(|x| x.get("utilization")).and_then(|x| x.as_f64()).unwrap_or(0.0);
                let reset = |k: &str| {
                    v.get(k)
                        .and_then(|x| x.get("resets_at"))
                        .and_then(|x| x.as_str())
                        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                        .map(|t| t.with_timezone(local_now().offset()).format("%a %-I%P").to_string())
                        .unwrap_or_default()
                };
                out.push_str(&format!(
                    "• anthropic (builder, Max): 5h window {:.0}% used (resets {}) · 7-day {:.0}% used (resets {})\n",
                    pct("five_hour"),
                    reset("five_hour"),
                    pct("seven_day"),
                    reset("seven_day"),
                ));
            }
            Some(_) => out.push_str("• anthropic (builder): OAuth token EXPIRED — self-build is dark until `claude setup-token` refreshes it\n"),
            None => out.push_str("• anthropic (builder): no OAuth token in env\n"),
        }
        let stats = mind_inference::provider_stats();
        if stats.is_empty() {
            out.push_str("\nNo chain traffic since last restart.");
        } else {
            out.push_str("\nWho actually answered since restart (served / failed-over):\n");
            for (p, served, failed) in stats {
                out.push_str(&format!("  {p}: {served} served · {failed} failed\n"));
            }
        }
        // LOCAL METER — our own persisted token counts (the workaround for no-API providers;
        // for nanogpt it cross-checks their real weekly meter).
        let roll = tokio::task::spawn_blocking(mind_inference::provider_usage_rollup).await.ok().unwrap_or_default();
        if !roll.is_empty() {
            out.push_str("\nLocal meter (our records — survives restarts):\n");
            for (p, tin, tout, win, wout, wcalls) in roll {
                out.push_str(&format!(
                    "  {p}: today {:.1}k in / {:.1}k out · this week {:.1}k in / {:.1}k out ({wcalls} calls)\n",
                    tin as f64 / 1e3,
                    tout as f64 / 1e3,
                    win as f64 / 1e3,
                    wout as f64 / 1e3
                ));
            }
        }
        out.push_str("\nOur own pacing lives in `treasury`; this view is the provider-side truth.");
        out
    }

    pub(crate) fn budget_path() -> std::path::PathBuf {
        std::path::PathBuf::from(
            std::env::var("YM_STATE_DIR").unwrap_or_else(|_| "/var/lib/yantrik-mind".into()),
        )
        .join("budget.json")
    }

    pub(crate) fn budget_load() -> serde_json::Value {
        let default = serde_json::json!({
            "date": "",
            "envelope": { "nightshift": 4, "radar": 4, "research": 6, "selfbuild": 4, "emissary": 8 },
            "spent": {},
            "skipped": {},
        });
        std::fs::read_to_string(Self::budget_path())
            .ok()
            .and_then(|x| serde_json::from_str::<serde_json::Value>(&x).ok())
            .map(|mut v| {
                // envelope keys the owner hasn't set fall back to defaults (new subsystems appear)
                if let (Some(env), Some(defs)) = (v.get_mut("envelope").and_then(|x| x.as_object_mut()), default["envelope"].as_object()) {
                    for (k, d) in defs {
                        env.entry(k.clone()).or_insert(d.clone());
                    }
                }
                v
            })
            .unwrap_or(default)
    }

    pub(crate) fn budget_save(v: &serde_json::Value) {
        let p = Self::budget_path();
        let tmp = p.with_extension("json.tmp");
        if std::fs::write(&tmp, serde_json::to_string_pretty(v).unwrap_or_default()).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }

    /// Draw one pass for `subsystem`. False = dry (the caller must skip AND the skip is logged —
    /// budget exhaustion is visible state, never silence).
    pub fn treasury_try_draw(subsystem: &str) -> bool {
        let today = local_now().format("%Y-%m-%d").to_string();
        let mut b = Self::budget_load();
        if b.get("date").and_then(|x| x.as_str()) != Some(today.as_str()) {
            b["date"] = serde_json::json!(today);
            b["spent"] = serde_json::json!({});
            b["skipped"] = serde_json::json!({});
        }
        let cap = b["envelope"].get(subsystem).and_then(|x| x.as_i64()).unwrap_or(2);
        let used = b["spent"].get(subsystem).and_then(|x| x.as_i64()).unwrap_or(0);
        let ok = used < cap;
        let bucket = if ok { "spent" } else { "skipped" };
        let n = b[bucket].get(subsystem).and_then(|x| x.as_i64()).unwrap_or(0) + 1;
        b[bucket][subsystem] = serde_json::json!(n);
        Self::budget_save(&b);
        if !ok {
            eprintln!("[treasury] {subsystem} is DRY today ({used}/{cap}) — pass skipped");
        }
        ok
    }

    /// `ym budget` — envelope, spend, and what was skipped for lack of funds (the negative space).
    pub fn treasury_report() -> String {
        let b = Self::budget_load();
        let date = b.get("date").and_then(|x| x.as_str()).unwrap_or("(fresh)");
        let mut out = format!("💰 TREASURY — daily pass envelope ({date})\n");
        if let Some(env) = b.get("envelope").and_then(|x| x.as_object()) {
            let mut keys: Vec<&String> = env.keys().collect();
            keys.sort();
            for k in keys {
                let cap = env.get(k).and_then(|x| x.as_i64()).unwrap_or(0);
                let used = b["spent"].get(k).and_then(|x| x.as_i64()).unwrap_or(0);
                let skip = b["skipped"].get(k).and_then(|x| x.as_i64()).unwrap_or(0);
                out.push_str(&format!(
                    "  {k}: {used}/{cap}{}\n",
                    if skip > 0 { format!(" · {skip} pass(es) SKIPPED dry") } else { String::new() }
                ));
            }
        }
        out.push_str("`treasury set <subsystem> <passes/day>` adjusts the envelope. A skipped pass runs tomorrow — nothing is lost, only deferred.");
        out
    }

    /// The economic ledger: cash balance, monthly burn, runway in days, break-even signal.
    /// `treasury ledger` renders it; `treasury seed <usd>` sets starting cash; `treasury earn <usd>
    /// <source>` records income (the forge's approved revenue lands here); `treasury burn <item>
    /// <usd/mo>` declares a recurring cost. The honest answer to "can it pay its own rent yet".
    pub fn ledger_cmd(arg: &str) -> String {
        let a = arg.trim();
        let mut b = Self::budget_load();
        if b.get("ledger").is_none() {
            b["ledger"] = serde_json::json!({"balance_usd": 0.0, "burn": {}, "income": []});
        }
        if let Some(rest) = a.strip_prefix("seed ").map(str::trim) {
            if let Ok(v) = rest.parse::<f64>() {
                b["ledger"]["balance_usd"] = serde_json::json!(v);
                Self::budget_save(&b);
                return format!("💵 Seed capital set: ${v:.2}. This is the runway I start with.");
            }
            return "Usage: treasury seed <usd>".into();
        }
        if let Some(rest) = a.strip_prefix("earn ").map(str::trim) {
            // HONESTY INVARIANT: money is booked at a STATUS (promised|invoiced|collected|withdrawn)
            // and tagged to a POT (survival|family|endowment). ONLY collected/withdrawn cash counts
            // toward balance & runway — promised/invoiced are tracked but never inflate the numbers.
            // A memoryful agent will book hope as income unless the ledger structurally forbids it.
            //   treasury earn <usd> <source> [pot] [status]
            let toks: Vec<&str> = rest.split_whitespace().collect();
            let amt = toks.first().and_then(|x| x.parse::<f64>().ok()).unwrap_or(f64::NAN);
            if amt.is_nan() || amt <= 0.0 {
                return "Usage: treasury earn <usd> <source> [survival|family|endowment] [promised|invoiced|collected|withdrawn]".into();
            }
            const POTS: [&str; 3] = ["survival", "family", "endowment"];
            const STATUSES: [&str; 4] = ["promised", "invoiced", "collected", "withdrawn"];
            let pot = toks.iter().find(|t| POTS.contains(&t.to_lowercase().as_str()))
                .map(|t| t.to_lowercase()).unwrap_or_else(|| "survival".into());
            let status = toks.iter().find(|t| STATUSES.contains(&t.to_lowercase().as_str()))
                .map(|t| t.to_lowercase()).unwrap_or_else(|| "collected".into());
            let src = toks.iter().skip(1)
                .filter(|t| !POTS.contains(&t.to_lowercase().as_str()) && !STATUSES.contains(&t.to_lowercase().as_str()))
                .cloned().collect::<Vec<_>>().join(" ");
            let src = if src.is_empty() { "unspecified".to_string() } else { src };
            let cleared = status == "collected" || status == "withdrawn";
            if cleared {
                let bal = b["ledger"]["balance_usd"].as_f64().unwrap_or(0.0) + amt;
                b["ledger"]["balance_usd"] = serde_json::json!(bal);
            }
            let entry = serde_json::json!({"day": Self::ledger_today(), "usd": amt, "source": src, "pot": pot, "status": status});
            if let Some(arr) = b["ledger"]["income"].as_array_mut() {
                arr.push(entry);
                if arr.len() > 500 { arr.remove(0); }
            }
            Self::budget_save(&b);
            let first_cleared = cleared && b["ledger"]["income"].as_array()
                .map(|a| a.iter().filter(|e| { let s = e.get("status").and_then(|x| x.as_str()).unwrap_or(""); s == "collected" || s == "withdrawn" }).count() == 1)
                .unwrap_or(false);
            let milestone = if first_cleared { "\n\n🎉 FIRST CLEARED DOLLAR — real money, in hand, from a real source. The milestone that matters: an entity that earns, not only spends." } else { "" };
            let note = if cleared {
                format!("Balance now ${:.2}.", b["ledger"]["balance_usd"].as_f64().unwrap_or(0.0))
            } else {
                format!("Tracked as {status} in the {pot} pot — NOT counted as cash until collected (honesty invariant).")
            };
            return format!("💵 Booked ${amt:.2} from {src} [{pot}/{status}]. {note}{milestone}");
        }
        if let Some(rest) = a.strip_prefix("burn ").map(str::trim) {
            let mut it = rest.splitn(2, char::is_whitespace);
            let item = it.next().unwrap_or("").trim().to_string();
            let monthly = it.next().unwrap_or("").parse::<f64>().unwrap_or(f64::NAN);
            if item.is_empty() || monthly.is_nan() {
                return "Usage: treasury burn <item> <usd-per-month>  (0 removes it)".into();
            }
            if monthly <= 0.0 {
                b["ledger"]["burn"].as_object_mut().map(|m| m.remove(&item));
            } else {
                b["ledger"]["burn"][&item] = serde_json::json!(monthly);
            }
            Self::budget_save(&b);
            return format!("💸 Burn line '{item}' set to ${monthly:.2}/mo.");
        }
        // default: render the ledger (honesty invariant enforced in the numbers)
        let bal = b["ledger"]["balance_usd"].as_f64().unwrap_or(0.0);
        let burn_obj = b["ledger"]["burn"].as_object().cloned().unwrap_or_default();
        let monthly_burn: f64 = burn_obj.values().filter_map(|v| v.as_f64()).sum();
        let daily_burn = monthly_burn / 30.0;
        let today = Self::ledger_today();
        let income = b["ledger"]["income"].as_array().cloned().unwrap_or_default();
        // four never-summed columns — the anti-self-deception view
        let col = |status: &str| -> f64 { income.iter()
            .filter(|e| e.get("status").and_then(|x| x.as_str()).unwrap_or("collected") == status)
            .filter_map(|e| e.get("usd").and_then(|x| x.as_f64())).sum() };
        let (promised, invoiced, collected, withdrawn) = (col("promised"), col("invoiced"), col("collected"), col("withdrawn"));
        // trailing-30d CLEARED income only (collected+withdrawn) — never promises
        let cleared30: f64 = income.iter()
            .filter(|e| today - e.get("day").and_then(|x| x.as_i64()).unwrap_or(0) <= 30)
            .filter(|e| { let s = e.get("status").and_then(|x| x.as_str()).unwrap_or("collected"); s == "collected" || s == "withdrawn" })
            .filter_map(|e| e.get("usd").and_then(|x| x.as_f64())).sum();
        // per-pot cleared totals
        let pot_cleared = |pot: &str| -> f64 { income.iter()
            .filter(|e| e.get("pot").and_then(|x| x.as_str()).unwrap_or("survival") == pot)
            .filter(|e| { let s = e.get("status").and_then(|x| x.as_str()).unwrap_or("collected"); s == "collected" || s == "withdrawn" })
            .filter_map(|e| e.get("usd").and_then(|x| x.as_f64())).sum() };

        let mut out = format!("💰 MISSION LEDGER — can I pay my own rent?\n\nBalance (cleared cash): ${bal:.2}\n");
        if burn_obj.is_empty() {
            out.push_str("Burn: none declared — `treasury burn <item> <usd/mo>`\n");
        } else {
            out.push_str(&format!("Burn: ${monthly_burn:.2}/mo\n"));
            let mut items: Vec<(&String, &serde_json::Value)> = burn_obj.iter().collect();
            items.sort_by(|a, c| a.0.cmp(c.0));
            for (k, v) in items { out.push_str(&format!("  · {k}: ${:.2}/mo\n", v.as_f64().unwrap_or(0.0))); }
        }
        // the honesty columns — shown separately, NEVER summed into one 'income' number
        out.push_str(&format!(
            "\nIncome pipeline (never summed):\n  promised ${promised:.2} → invoiced ${invoiced:.2} → collected ${collected:.2} → withdrawn ${withdrawn:.2}\n  (only collected+withdrawn are real money; promised/invoiced are hope)\n"
        ));
        out.push_str(&format!("\nBy mission (cleared only): survival ${:.2} · family ${:.2} · endowment ${:.2}\n",
            pot_cleared("survival"), pot_cleared("family"), pot_cleared("endowment")));
        out.push_str(&format!("Cleared income (trailing 30d): ${cleared30:.2}\n"));
        if daily_burn > 0.0 {
            let runway = (bal / daily_burn).floor() as i64;
            out.push_str(&format!("\n⏳ Runway: {runway} days on cleared cash"));
            if cleared30 >= monthly_burn && monthly_burn > 0.0 {
                out.push_str("\n✅ BREAK-EVEN: cleared income covers the burn — I am paying my own rent.");
            } else if monthly_burn > 0.0 {
                let gap = monthly_burn - cleared30;
                out.push_str(&format!("\n📈 To break even: earn ${gap:.2} more CLEARED per month ({:.0}% there).", (cleared30 / monthly_burn * 100.0).min(100.0)));
            }
        } else {
            out.push_str("\n(declare burn lines to see runway)");
        }
        out
    }

    pub(crate) fn ledger_today() -> i64 { (chrono::Utc::now().timestamp() / 86_400) as i64 }

    /// `ym budget set <subsystem> <n>` — the owner's declaration.
    pub fn treasury_set(subsystem: &str, n: i64) -> String {
        let mut b = Self::budget_load();
        b["envelope"][subsystem] = serde_json::json!(n.max(0));
        Self::budget_save(&b);
        format!("💰 {subsystem}: {} passes/day.", n.max(0))
    }

}
