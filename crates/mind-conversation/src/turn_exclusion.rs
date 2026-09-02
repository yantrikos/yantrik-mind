//! L3a: turn exclusion for the process-hosted loop runner.
//!
//! The runner may start the offline-cognition pass only when no turn is in flight on ANY surface,
//! and the check must be atomic against a turn starting at the same instant. The contract is the
//! legacy one made explicit: a turn registered first wins and the pass does not start; a turn
//! arriving after admission proceeds without waiting and may overlap the already-running pass.
//! Nothing is cancelled and no turn ever waits on the pass — the only critical section is the
//! await-free admission itself.
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::RwLock;

pub struct TurnExclusion {
    /// Shared by turns for the instant they register; exclusive for the instant DMN is admitted.
    /// Never held across an await.
    admission: RwLock<()>,
    active_turns: AtomicUsize,
    /// Monotone: the latest user activity on any surface, in ms.
    last_user_activity_ms: AtomicU64,
    /// The most recent registration as ONE consistent pair — its stamp and its bounded surface
    /// label (`turn`, `fast_reply`, `cli:<verb>`) — updated under one lock so concurrent
    /// registrations can never pair one caller's stamp with another's label.
    last_registration: std::sync::Mutex<(u64, &'static str)>,
    dmn_running: AtomicBool,
}

/// Held for the whole life of one turn. Dropping it (normally or by cancellation) releases it.
pub struct TurnGuard<'a> {
    owner: &'a TurnExclusion,
    /// The activity stamp observed atomically when THIS turn registered. Keeping it on the guard
    /// prevents a concurrent turn from overwriting a diagnostic turn's evidence.
    previous_activity_ms: u64,
    /// The registration this one displaced, as one consistent (stamp, surface) pair — the
    /// caller that registered before us.
    previous_registration: (u64, &'static str),
}

impl TurnGuard<'_> {
    pub fn previous_activity_ms(&self) -> u64 {
        self.previous_activity_ms
    }
    /// The bounded label of the surface that registered before this turn.
    pub fn previous_surface(&self) -> &'static str {
        self.previous_registration.1
    }
    /// The registration before this turn, as the consistent pair it was written as.
    pub fn previous_registration(&self) -> (u64, &'static str) {
        self.previous_registration
    }
}

impl Drop for TurnGuard<'_> {
    fn drop(&mut self) {
        self.owner.active_turns.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Held for the life of one offline-cognition pass. Dropping it clears `dmn_running`.
pub struct DmnPermit<'a> {
    owner: &'a TurnExclusion,
}

impl Drop for DmnPermit<'_> {
    fn drop(&mut self) {
        self.owner.dmn_running.store(false, Ordering::Release);
    }
}

impl TurnExclusion {
    /// Seeded with the process's start: the legacy poll loop began with `last_activity =
    /// now_ms()`, so the first idle stretch is counted from boot, never from zero.
    pub fn starting_at(now_ms: u64) -> Self {
        Self {
            admission: RwLock::new(()),
            active_turns: AtomicUsize::new(0),
            last_user_activity_ms: AtomicU64::new(now_ms),
            last_registration: std::sync::Mutex::new((now_ms, "boot")),
            dmn_running: AtomicBool::new(false),
        }
    }

    /// Register a turn. Shared admission: never blocked by another turn, never blocked by a
    /// running pass — only by the microseconds of a DMN admission check in progress.
    pub fn begin_turn(&self, now_ms: u64) -> TurnGuard<'_> {
        self.begin_turn_on("turn", now_ms)
    }

    /// Register a turn and name its surface with a bounded static label (never content). A
    /// person's act: it counts AND moves the user-activity clock.
    pub fn begin_turn_on(&self, surface: &'static str, now_ms: u64) -> TurnGuard<'_> {
        self.register(surface, now_ms, true)
    }

    /// Register a MACHINE view — the cockpit's automatic JSON refreshes. It counts as an active
    /// turn (DMN never starts while it is in flight) but does not move the user-activity clock:
    /// a console tab left open is not a person being present.
    pub fn begin_view_on(&self, surface: &'static str, now_ms: u64) -> TurnGuard<'_> {
        self.register(surface, now_ms, false)
    }

    fn register(&self, surface: &'static str, now_ms: u64, moves_clock: bool) -> TurnGuard<'_> {
        let _shared = self.admission.read().unwrap_or_else(|p| p.into_inner());
        self.active_turns.fetch_add(1, Ordering::AcqRel);
        // The pair is swapped under its own lock: the displaced (stamp, surface) belongs to
        // exactly one earlier registration, never to a mixture of two.
        let previous = {
            let mut last = self
                .last_registration
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            std::mem::replace(&mut *last, (now_ms, surface))
        };
        let before = if moves_clock {
            self.last_user_activity_ms
                .fetch_max(now_ms, Ordering::AcqRel)
        } else {
            self.last_user_activity_ms.load(Ordering::Acquire)
        };
        TurnGuard {
            owner: self,
            previous_activity_ms: before,
            previous_registration: previous,
        }
    }

    /// The bounded label of the surface that registered most recently.
    pub fn last_surface(&self) -> &'static str {
        self.last_registration
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .1
    }

    /// Admit the offline-cognition pass iff no turn is active, the idle stretch is met, and no
    /// pass is already running — all re-checked under the exclusive lock, which is released
    /// before this returns so the pass itself never holds it.
    pub fn try_admit_dmn(&self, now_ms: u64, idle_ms: u64) -> Option<DmnPermit<'_>> {
        let _exclusive = self.admission.write().unwrap_or_else(|p| p.into_inner());
        if self.active_turns.load(Ordering::Acquire) != 0 {
            return None;
        }
        if now_ms.saturating_sub(self.last_user_activity_ms.load(Ordering::Acquire)) < idle_ms {
            return None;
        }
        if self.dmn_running.swap(true, Ordering::AcqRel) {
            return None;
        }
        Some(DmnPermit { owner: self })
    }

    pub fn active_turns(&self) -> usize {
        self.active_turns.load(Ordering::Acquire)
    }
    pub fn last_user_activity_ms(&self) -> u64 {
        self.last_user_activity_ms.load(Ordering::Acquire)
    }
    pub fn dmn_running(&self) -> bool {
        self.dmn_running.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;

    const IDLE: u64 = 600_000;

    /// Every production reply surface of the engine registers a turn for its whole life:
    /// `turn` (the agentic path), `fast_reply` (the voice fast path), `cli_dispatch` (the
    /// operator console). `handle_turn` / `handle_turn_as` are reached only through those; they
    /// take no guard of their own (that would double-count), and mind-core's callsite fixture
    /// asserts no frontend calls them directly.
    #[test]
    fn every_production_reply_surface_registers_a_turn() {
        let lib = include_str!("lib.rs");
        let cognitive = include_str!("cognitive.rs");
        fn body_after<'a>(src: &'a str, signature: &str) -> &'a str {
            let start = src.find(signature).unwrap_or_else(|| panic!("{signature}"));
            // The function's own extent: up to the next item at the same indentation.
            let end = src[start + 1..]
                .find(
                    "
    pub ",
                )
                .map(|i| start + 1 + i)
                .unwrap_or(src.len());
            &src[start..end]
        }
        for (src, signature) in [
            (
                cognitive,
                "pub async fn turn(self: &Arc<Self>, user_text: &str, id: TurnIdentity)",
            ),
            (
                lib,
                "pub async fn fast_reply(&self, user_text: &str, id: TurnIdentity)",
            ),
            (lib, "async fn cli_dispatch_inner("),
        ] {
            assert!(
                body_after(src, signature).contains(".begin_turn"),
                "{signature} must register a turn"
            );
        }
        // The two console wrappers reach the registering inner dispatch and register nothing
        // of their own; the view wrapper enforces the allowlist before choosing the origin.
        for wrapper in [
            "pub async fn cli_dispatch(",
            "pub async fn cli_dispatch_view(",
        ] {
            let body = body_after(lib, wrapper);
            assert!(
                body.contains("cli_dispatch_inner("),
                "{wrapper} reaches the inner dispatch"
            );
            assert!(
                !body.contains("begin_"),
                "{wrapper} registers nothing of its own"
            );
        }
        assert!(body_after(lib, "pub async fn cli_dispatch_view(").contains("is_machine_view("));
        for signature in [
            "pub async fn handle_turn(&self, user_text: &str)",
            "pub async fn handle_turn_as(&self, user_text: &str, id: TurnIdentity)",
        ] {
            assert!(
                !body_after(lib, signature).contains("begin_turn"),
                "{signature} is reached through a registering entry; it must not double-count"
            );
        }
    }

    /// Boot is activity: the first runner tick after start cannot admit DMN until the idle
    /// stretch has passed since the process started (legacy `last_activity = now_ms()`).
    /// Each registration names its surface with a bounded static label, so a readout can say
    /// who registered before it; the default `begin_turn` is the agentic `turn`.
    #[test]
    fn a_registration_names_its_surface_with_a_bounded_label() {
        let x = TurnExclusion::starting_at(0);
        assert_eq!(x.last_surface(), "boot");
        let a = x.begin_turn_on("cli:why", 10);
        assert_eq!(
            a.previous_surface(),
            "boot",
            "the diagnostic sees who came before it"
        );
        assert_eq!(x.last_surface(), "cli:why");
        let b = x.begin_turn(20);
        assert_eq!(b.previous_surface(), "cli:why");
        assert_eq!(x.last_surface(), "turn");
        drop(a);
        drop(b);
        assert_eq!(
            x.last_surface(),
            "turn",
            "the label is who registered last, not who is active"
        );
    }

    /// A machine view counts as a turn while it lives but does not move the user-activity
    /// clock; a typed line does. DMN is admitted after a view when the person has been idle.
    #[test]
    fn a_machine_view_counts_as_a_turn_but_is_not_user_activity() {
        let x = TurnExclusion::starting_at(1_000);
        let view = x.begin_view_on("cli:loops_json", 500_000);
        assert_eq!(x.active_turns(), 1, "the view counts while it lives");
        assert_eq!(x.last_user_activity_ms(), 1_000, "the clock did not move");
        assert_eq!(view.previous_activity_ms(), 1_000);
        assert!(
            x.try_admit_dmn(700_000, IDLE).is_none(),
            "not while the view is in flight"
        );
        drop(view);
        assert!(
            x.try_admit_dmn(700_000, IDLE).is_some(),
            "idle since boot, views notwithstanding"
        );
        let typed = x.begin_turn_on("cli:why", 800_000);
        drop(typed);
        assert_eq!(x.last_user_activity_ms(), 800_000, "a typed line moves it");
        assert!(x.try_admit_dmn(900_000, IDLE).is_none());
    }

    /// The machine entry is fail-closed over the exact read-only shapes emitted by the web GET
    /// routes. Near-misses become person activity instead of silently hiding from the idle clock.
    #[test]
    fn the_machine_view_allowlist_is_exact() {
        for line in [
            "jobs json",
            "horizons_json",
            "horizon_history_json goal:horizon:abc-1_ok",
            "chains_json",
            "chains_json since=start",
            "chains_json since=1788324090623",
            "skills_json",
            "claims_json",
            "loops_json",
            "orders",
            "orders json",
        ] {
            assert!(
                crate::ConversationEngine::is_machine_view(line),
                "web GET view must remain a machine view: {line:?}"
            );
        }

        for line in [
            "",
            "jobs",
            "jobs json extra",
            "horizons_json extra",
            "horizon_history_json",
            "horizon_history_json goal.with.dot",
            "horizon_history_json goal:horizon:a extra",
            "chains_json since=",
            "chains_json since=123 extra",
            "chains_json since=123-456",
            "orders extra",
            "delegate helper: do work",
            "plugin enable finance",
        ] {
            assert!(
                !crate::ConversationEngine::is_machine_view(line),
                "near-miss or mutation must count as person activity: {line:?}"
            );
        }

        let overlong_id = format!("horizon_history_json {}", "a".repeat(65));
        assert!(!crate::ConversationEngine::is_machine_view(&overlong_id));
    }

    /// Under concurrent registration, every guard's displaced pair is exactly one earlier
    /// registration's (stamp, surface) — attribution cannot tear into a mixture.
    #[test]
    fn concurrent_registrations_cannot_tear_the_attribution_pair() {
        use std::collections::BTreeSet;
        use std::sync::Arc;
        let x = Arc::new(TurnExclusion::starting_at(0));
        let labels: [&'static str; 4] = ["turn", "fast_reply", "cli:why", "cli:loops_json"];
        let handles: Vec<_> = (1..=64u64)
            .map(|i| {
                let x = x.clone();
                let label = labels[(i % 4) as usize];
                std::thread::spawn(move || {
                    let g = x.begin_turn_on(label, i * 1_000);
                    ((i * 1_000, label), g.previous_registration())
                })
            })
            .collect();
        let mut written = BTreeSet::from([(0u64, "boot")]);
        let mut displaced = Vec::new();
        for h in handles {
            let (mine, prev) = h.join().unwrap();
            written.insert(mine);
            displaced.push(prev);
        }
        for pair in displaced {
            assert!(
                written.contains(&pair),
                "displaced pair {pair:?} was never written as one"
            );
        }
    }

    #[test]
    fn nothing_is_admitted_before_the_idle_stretch_has_passed_since_boot() {
        let boot = 5_000_000;
        let x = TurnExclusion::starting_at(boot);
        assert!(x.try_admit_dmn(boot + 5_000, IDLE).is_none());
        assert!(x.try_admit_dmn(boot + IDLE - 1, IDLE).is_none());
        assert!(x.try_admit_dmn(boot + IDLE, IDLE).is_some());
    }

    #[test]
    fn a_turn_registered_first_keeps_the_pass_from_starting_for_its_whole_life() {
        let x = TurnExclusion::starting_at(0);
        // Idle long enough, no turn: admitted.
        assert!(x.try_admit_dmn(1_000_000, IDLE).is_some());
        // A long turn holds its guard across the "await": no admission while it lives.
        let guard = x.begin_turn(1_000_000);
        assert_eq!(x.active_turns(), 1);
        assert!(x.try_admit_dmn(2_000_000, IDLE).is_none());
        assert!(x.try_admit_dmn(3_000_000, IDLE).is_none());
        drop(guard);
        assert_eq!(x.active_turns(), 0);
        // The turn moved the activity clock: not idle yet, so still not admitted...
        assert!(x.try_admit_dmn(1_000_000 + IDLE - 1, IDLE).is_none());
        // ...until the idle stretch has passed.
        assert!(x.try_admit_dmn(1_000_000 + IDLE, IDLE).is_some());
    }

    #[test]
    fn overlapping_turns_count_two_one_zero_and_a_cancelled_turn_releases_by_drop() {
        let x = TurnExclusion::starting_at(0);
        let a = x.begin_turn(10);
        let b = x.begin_turn(20);
        assert_eq!(x.active_turns(), 2);
        drop(a);
        assert_eq!(x.active_turns(), 1);
        // A cancelled turn is a dropped future that had already registered: poll it once so
        // the guard is taken, then drop it — the count must return to what it was.
        let mut cancelled = Box::pin(async {
            let _g = x.begin_turn(30);
            std::future::pending::<()>().await;
        });
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(&waker);
        assert!(cancelled.as_mut().poll(&mut cx).is_pending());
        assert_eq!(
            x.active_turns(),
            2,
            "the cancelled turn registered while pending"
        );
        drop(cancelled);
        assert_eq!(
            x.active_turns(),
            1,
            "dropping the pending future released its guard"
        );
        drop(b);
        assert_eq!(x.active_turns(), 0);
        assert_eq!(x.last_user_activity_ms(), 30);
    }

    /// A diagnostic turn owns the activity stamp it observed before registration. A second turn
    /// may register before the diagnostic renders, but cannot overwrite the diagnostic's snapshot.
    #[test]
    fn overlapping_turns_keep_independent_previous_activity_snapshots() {
        let x = TurnExclusion::starting_at(100);
        let diagnostic = x.begin_turn(1_000);
        let concurrent = x.begin_turn(2_000);

        assert_eq!(diagnostic.previous_activity_ms(), 100);
        assert_eq!(concurrent.previous_activity_ms(), 1_000);
        assert_eq!(x.last_user_activity_ms(), 2_000);
        assert_eq!(x.active_turns(), 2);

        drop(concurrent);
        drop(diagnostic);
        assert_eq!(x.active_turns(), 0);
    }

    /// The two frozen interleavings of the race.
    #[test]
    fn the_race_interleavings_are_exactly_the_contract() {
        // (i) The turn registers BEFORE admission: admission observes it atomically, no pass.
        let x = TurnExclusion::starting_at(0);
        let turn = x.begin_turn(1);
        assert!(x.try_admit_dmn(1 + IDLE, IDLE).is_none());
        drop(turn);
        // (ii) The turn arrives AFTER admission: it proceeds without waiting, the pass keeps its
        // permit and runs to completion; overlap is allowed by contract.
        let permit = x
            .try_admit_dmn(2 + IDLE, IDLE)
            .expect("admitted while idle");
        assert!(x.dmn_running());
        let turn = x.begin_turn(3 + IDLE);
        assert_eq!(x.active_turns(), 1, "the turn was not made to wait");
        assert!(x.dmn_running(), "the pass was not cancelled");
        // No second pass while one runs, whatever the turn count.
        drop(turn);
        assert!(x.try_admit_dmn(4 + IDLE, IDLE).is_none());
        drop(permit);
        assert!(!x.dmn_running());
        // A fresh admission is possible again only once idle from the turn's activity.
        assert!(x.try_admit_dmn(3 + IDLE + IDLE, IDLE).is_some());
    }

    /// Admission is atomic against a turn starting on another thread: whichever takes the lock
    /// first decides, and no interleaving admits a pass while a registered turn exists.
    #[test]
    fn admission_is_atomic_against_concurrent_turn_registration() {
        use std::sync::Arc;
        let x = Arc::new(TurnExclusion::starting_at(0));
        let mut violations = 0usize;
        for round in 0..2_000u64 {
            let now = 10_000_000 + round * 1_000_000;
            let xa = x.clone();
            let t = std::thread::spawn(move || {
                let g = xa.begin_turn(now);
                // Hold the turn briefly so an admission racing with registration must see it.
                std::hint::black_box(&g);
                let admitted_during_turn = xa.try_admit_dmn(now + IDLE, IDLE).is_some();
                drop(g);
                admitted_during_turn
            });
            let admitted_here = x.try_admit_dmn(now + IDLE, IDLE);
            let admitted_in_turn = t.join().unwrap();
            // The turn thread can never be admitted while it holds its own guard.
            if admitted_in_turn {
                violations += 1;
            }
            // If this thread was admitted, it was admitted before the turn registered
            // (allowed interleaving ii); it must have seen zero active turns at that instant.
            drop(admitted_here);
        }
        assert_eq!(violations, 0);
    }
}
