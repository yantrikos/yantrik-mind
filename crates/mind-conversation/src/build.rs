//! E.RUNG8: carry one of this mind's OWN proposals to a diff, and let something other than the
//! builder decide whether that diff is worth anything.
//!
//! Everything below the proposal was already shipped and witnessed — notice a repository has
//! moved, research it, ground the reading in the real commit, spool a typed proposal carrying an
//! acceptance test. Nothing read the spool back. `derive_work_proposal` still says so in its own
//! doc comment: *"this shadow path never builds or executes proposals."*
//!
//! The hard part is not running a builder. It is that the builder is an agent which runs its own
//! tests and then reports how it went, and believing that sentence is the exact failure this
//! ladder exists to catch. So the acceptance test is run by the MIND, twice, in the sandbox, on
//! throwaway copies: once against the pristine tree and once against the built one.
//!
//! **The differential gate.** If the pristine tree already passes, the build does not happen at
//! all. A check that passes before the change proves nothing about the change, and spending the
//! coder on it would buy a green that means nothing. That is the banked rule — a test written to
//! prove a fix is not evidence until it has been watched to fail — turned on the mind's own work.
//!
//! Nothing here commits, pushes, or opens a pull request, and nothing writes to the shared
//! checkout the field scans read: the build happens in a clone made for it and thrown away with it.

use crate::ProjectProposal;
use mind_tools::sandbox::Limits;
use std::path::Path;

/// Where the acceptance test's tree is seeded inside the sandbox scratch. A subdirectory, because
/// the sandbox owns `run.sh` at the scratch root and a repository may have one of its own.
const SEED_DIR: &str = "tree";

/// The file the check is written to, beside the seeded tree rather than inside it — a repository
/// must not be able to have a file of this name and thereby replace the check with its own.
const CHECK_FILE: &str = "ym_check.sh";

/// One acceptance-test run. Generous enough for a real test suite, short enough that a runaway
/// dies without anybody watching it.
fn check_limits() -> Limits {
    Limits {
        wall_secs: 180,
        cpu_secs: 150,
        mem_bytes: 2048 * 1024 * 1024,
        max_procs: 256,
        fsize_bytes: 64 * 1024 * 1024,
    }
}

/// What the coding agent leaves in the workdir that is ITS OWN rather than the repository's.
///
/// E.RUNG8b, from the first real run. The coder points `HOME` at the workdir on purpose, so a run
/// cannot reach the service user's real home — and the cost is that the agent's own state lands
/// inside the very tree the build is measured on. Two distinct harms, both observed: a diff that
/// was 2,727 lines of session log, tool-result cache and backups for a run that changed nothing at
/// all, so `NoChange` could never be reported; and a `.key` file copied into a sandbox that runs a
/// MODEL-AUTHORED shell command, where a check could read it and print it into the reply.
///
/// One list, two derived uses — the git exclusion and the sandbox skip. They were nearly two
/// hand-maintained lists, which is the seam E.HORIZON1 spent a day learning not to leave open.
const AGENT_HOME: [&str; 6] = [
    ".claude",
    ".claude.json",
    ".config",
    ".cache",
    ".local",
    ".npm",
];

/// The same set as gitignore patterns, anchored at the workdir root so a `.claude/` that belongs to
/// the repository's own source is untouched.
pub(crate) fn agent_home_patterns() -> Vec<String> {
    AGENT_HOME.iter().map(|name| format!("/{name}")).collect()
}

/// How much of the diff the reply carries. The whole of it stays on disk in the workdir.
const DIFF_SHOWN: usize = 2200;

/// What a build attempt came to. Every variant is a result; exactly one of them is a pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BuildOutcome {
    /// The pristine tree already passes the acceptance test, so the test can certify nothing.
    /// The proposal is at fault here, not the builder — and no build was run.
    NotADifferential,
    /// The builder changed nothing.
    NoChange,
    /// Failed before, passes after. The only outcome that is rung 8.
    Verified,
    /// A real attempt that the check still refuses. The diff is worth reading anyway.
    Unverified,
}

impl BuildOutcome {
    pub(crate) fn headline(self) -> &'static str {
        match self {
            Self::NotADifferential => {
                "NOT A DIFFERENTIAL — the check already passed before anything was built, so it \
                 cannot certify a change. Nothing was built: the proposal needs a check that fails \
                 first."
            }
            Self::NoChange => "NO CHANGE — the builder left the tree as it found it.",
            Self::Verified => {
                "VERIFIED — the check failed on the pristine tree and passes on the built one, and \
                 both runs were the mind's, not the builder's."
            }
            Self::Unverified => {
                "UNVERIFIED — a real diff, and the check still refuses it. Shown anyway; a failed \
                 attempt is worth more than a hidden one."
            }
        }
    }
}

/// What the run against the PRISTINE tree licenses.
///
/// `Some(_)` stops the pipeline before the coder is ever spent. A timed-out run is a failure like
/// any other non-zero exit, so it does NOT stop anything: the check demonstrably does not pass on
/// the unchanged tree, which is all this gate asks.
pub(crate) fn differential_gate(before_exit: i32, before_timed_out: bool) -> Option<BuildOutcome> {
    (!before_timed_out && before_exit == 0).then_some(BuildOutcome::NotADifferential)
}

/// The verdict, once there is a diff and a second run to read.
pub(crate) fn built_outcome(diff: &str, after_exit: i32, after_timed_out: bool) -> BuildOutcome {
    if diff.trim().is_empty() {
        return BuildOutcome::NoChange;
    }
    // A run killed at the wall clock never passes. It is the shape a hung check takes, and reading
    // it as success would hand a green to precisely the check that could not finish.
    if after_timed_out || after_exit != 0 {
        return BuildOutcome::Unverified;
    }
    BuildOutcome::Verified
}

/// How the builder's OWN run ended.
///
/// E.RUNG8c: run 2 came back `NO CHANGE — the builder left the tree as it found it`, which reads
/// as a builder that ran and declined. It had in fact spent eleven minutes reading, converged on a
/// plan, and was three sentences into designing the change when its provider answered
/// `429 Too Many Requests` and the round ended. A verdict that cannot tell those two apart blames
/// the builder for the lane, and the machinery to tell them apart (`provider_refusal`) already
/// existed and simply was not being read here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BuilderEnd {
    /// It ran to the end of its own turn.
    Completed,
    /// The wall clock expired while it was still working.
    WallClock,
    /// The provider refused it — quota, credentials, or the endpoint.
    Refused(&'static str),
}

/// A refusal outranks a timeout: a round killed at the clock *after* a 429 was stopped by the 429.
pub(crate) fn builder_end(summary: &str, timed_out: bool) -> BuilderEnd {
    if let Some(reason) = mind_tools::coder::provider_refusal(summary) {
        return BuilderEnd::Refused(reason);
    }
    if timed_out {
        BuilderEnd::WallClock
    } else {
        BuilderEnd::Completed
    }
}

/// The verdict sentence, which may not claim more than the builder's own ending supports.
pub(crate) fn verdict_line(outcome: BuildOutcome, end: &BuilderEnd) -> String {
    match (outcome, end) {
        (BuildOutcome::NoChange, BuilderEnd::Refused(reason)) => format!(
            "NO CHANGE, AND NOT THE BUILDER'S DOING — its provider refused it ({reason}) before it \
             wrote anything. This says nothing about whether the goal was buildable; the lane ran \
             out, not the builder."
        ),
        (BuildOutcome::NoChange, BuilderEnd::WallClock) => {
            "NO CHANGE — the builder was still working when its wall clock expired. It did not \
             decline the goal; it ran out of time before touching the tree."
                .to_string()
        }
        (BuildOutcome::Unverified, BuilderEnd::Refused(reason)) => format!(
            "UNVERIFIED, ON A PARTIAL BUILD — the check refuses this diff, and the builder was cut \
             off by its provider ({reason}) rather than finishing. Read the diff as an unfinished \
             attempt."
        ),
        (BuildOutcome::Unverified, BuilderEnd::WallClock) => {
            "UNVERIFIED, ON A PARTIAL BUILD — the check refuses this diff, and the builder's wall \
             clock expired before it finished. Read the diff as an unfinished attempt."
                .to_string()
        }
        (other, _) => other.headline().to_string(),
    }
}

/// The verdict, reconciled against the pristine run that licensed it.
///
/// `differential_gate` stops the pipeline, but a stop is a CALL, and a call can be deleted. This is
/// the same rule enforced where the verdict is rendered rather than where it is decided: a check
/// that passed on the unchanged tree yields `NotADifferential` whatever the pipeline computed, so
/// no rewiring of the steps can produce a `VERIFIED` that its own first exit code contradicts.
pub(crate) fn reconcile(
    before_exit: i32,
    before_timed_out: bool,
    outcome: BuildOutcome,
) -> BuildOutcome {
    match differential_gate(before_exit, before_timed_out) {
        Some(forced) => forced,
        None => outcome,
    }
}

/// Files touched, lines added, lines removed — read off a unified diff.
pub(crate) fn diff_shape(diff: &str) -> (usize, usize, usize) {
    let mut files = 0usize;
    let mut added = 0usize;
    let mut removed = 0usize;
    for line in diff.lines() {
        if line.starts_with("+++ ") {
            files += 1;
        } else if line.starts_with("+") && !line.starts_with("+++") {
            added += 1;
        } else if line.starts_with("-") && !line.starts_with("---") {
            removed += 1;
        }
    }
    (files, added, removed)
}

/// The newest spooled proposal, for one repository or across all of them.
pub(crate) fn newest_proposal(dir: &Path, repo: Option<&str>) -> Option<ProjectProposal> {
    let mut best: Option<(std::time::SystemTime, ProjectProposal)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(proposal) = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|json| ProjectProposal::from_json(&json))
        else {
            continue;
        };
        if repo.is_some_and(|want| !proposal.repo.eq_ignore_ascii_case(want)) {
            continue;
        }
        let when = path
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        match &best {
            Some((seen, _)) if *seen >= when => {}
            _ => best = Some((when, proposal)),
        }
    }
    best.map(|(_, proposal)| proposal)
}

/// What the builder is told. The last paragraph is the point of the whole rung: its own account of
/// how the run went is not what decides.
pub(crate) fn build_task(proposal: &ProjectProposal) -> String {
    format!(
        "You are in a checkout of {repo} at commit {sha}.\n\n\
         GOAL — this is the whole task, and nothing beyond it:\n{goal}\n\n\
         DEFINITION OF DONE — this exact command must go from failing to passing:\n{check}\n\n\
         RULES:\n\
         - Change as little as possible. Do not reformat, refactor or tidy anything the goal does \
         not require.\n\
         - Do not run `git commit`, `git push`, or change branches. Leave your work in the working \
         tree.\n\
         - The command above will be run again by something other than you, in a network-isolated \
         sandbox, against a copy of this tree. Your own report of how it went is not what decides, \
         so do not describe success — produce it.",
        repo = proposal.repo,
        sha = proposal.base_sha,
        goal = proposal.goal,
        check = proposal.acceptance_test,
    )
}

/// The reply. Pure, so the shape of a verdict can be pinned without a Linux box under it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_build(
    proposal: &ProjectProposal,
    before_exit: i32,
    before_timed_out: bool,
    outcome: BuildOutcome,
    after: Option<(i32, bool)>,
    diff: &str,
    check_output: &str,
    workdir: &str,
    hidden: usize,
    end: &BuilderEnd,
    builder_words: &str,
) -> String {
    let outcome = reconcile(before_exit, before_timed_out, outcome);
    let mut out = format!(
        "🔨 BUILD — {repo} @ {sha}\n\nGOAL: {goal}\nCHECK: {check}\n\n",
        repo = proposal.repo,
        sha = proposal.base_sha,
        goal = proposal.goal,
        check = proposal.acceptance_test,
    );
    out.push_str(&format!(
        "pristine tree: exit {before_exit}{}{}\n",
        if before_timed_out { " (timed out)" } else { "" },
        if before_exit == 0 && !before_timed_out {
            "  ← it should have FAILED here"
        } else {
            "  ← failed, as a check for an unmade change must"
        }
    ));
    match after {
        Some((exit, timed_out)) => out.push_str(&format!(
            "built tree:    exit {exit}{}\n",
            if timed_out { " (timed out)" } else { "" }
        )),
        None => out.push_str("built tree:    not run — no build happened\n"),
    }
    if !diff.trim().is_empty() {
        let (files, added, removed) = diff_shape(diff);
        out.push_str(&format!(
            "diff:          {files} file(s), +{added} −{removed}\n"
        ));
    }
    out.push_str(&format!(
        "builder:       {}\n",
        match end {
            BuilderEnd::Completed => "ran to the end of its own turn".to_string(),
            BuilderEnd::WallClock => "cut off at the wall clock, still working".to_string(),
            BuilderEnd::Refused(reason) => format!("refused by its provider — {reason}"),
        }
    ));
    out.push_str(&format!("\nVERDICT: {}\n", verdict_line(outcome, end)));

    let tail = check_output.trim();
    if !tail.is_empty() {
        let shown: String = tail.chars().rev().take(600).collect::<Vec<_>>().into_iter().rev().collect();
        out.push_str(&format!("\nWHAT THE CHECK SAID:\n{shown}\n"));
    }
    let words = builder_words.trim();
    if !words.is_empty() {
        let shown: String = words.chars().rev().take(500).collect::<Vec<_>>().into_iter().rev().collect();
        out.push_str(&format!(
            "\nWHAT THE BUILDER SAID (its own account, which is not what decides):\n{shown}\n"
        ));
    }
    if !diff.trim().is_empty() {
        let shown: String = diff.chars().take(DIFF_SHOWN).collect();
        let more = diff.chars().count().saturating_sub(shown.chars().count());
        out.push_str(&format!(
            "\nDIFF:\n{shown}{}\n",
            if more > 0 {
                format!("\n… {more} more characters; the whole tree is at {workdir}")
            } else {
                String::new()
            }
        ));
    }
    if hidden > 0 {
        out.push_str(&format!(
            "\n{hidden} path(s) are excluded from this diff: the coding agent's own home, which \
             lands in the workdir because the coder points HOME there. Its notes about making a \
             change are not the change.\n"
        ));
    }
    out.push_str(
        "\nNothing was committed, pushed, or opened as a pull request, and the checkout the field \
         scans read was not written to.",
    );
    out
}

impl crate::ConversationEngine {
    /// `work build [<repo>]` — E.RUNG8, explicitly invoked.
    ///
    /// Never reached from the nightly scan: WorkOps spools proposals and does not command builds.
    pub(crate) async fn work_build(&self, arg: &str) -> String {
        let want = arg.trim();
        let dir = Path::new(crate::PROJECT_PROPOSALS_DIR);
        let Some(proposal) = newest_proposal(dir, (!want.is_empty()).then_some(want)) else {
            return if want.is_empty() {
                "🔨 Nothing spooled to build. `work run` writes a proposal when a watched \
                 repository has moved."
                    .to_string()
            } else {
                format!("🔨 No pending proposal for \"{want}\". `proposals` lists what is spooled.")
            };
        };

        let Some(coder) = self.coder.clone() else {
            return "🔨 No coder lane here, so nothing can be built. The proposal stays spooled."
                .to_string();
        };
        if let Some(dead) = self.coder_dead_reason() {
            return format!("🔨 Not building — {dead}. The proposal stays spooled.");
        }

        // The check is model-authored, so the sandbox is not optional. An unverified diff is not
        // what rung 8 asks for, and running an unsandboxed check to get one would be worse.
        let sandbox = mind_tools::Sandbox::new().hiding(crate::syntax::state_dir());
        if !sandbox.available().await {
            return "🔨 Not building — the sandbox is unavailable here, and the acceptance test is \
                    the mind's own writing. It does not run outside one."
                .to_string();
        }

        let Some(url) = self.repo_for_subject(&proposal.repo).await else {
            return format!(
                "🔨 No repository configured for \"{}\", so its proposal cannot be built. \
                 `code add <url>` registers one.",
                proposal.repo
            );
        };
        let name = mind_tools::code::repo_name(&url);
        let Some(checkout) = mind_tools::code::checkout_dir(&name) else {
            return format!("🔨 \"{name}\" has no local checkout yet. `work run` syncs one.");
        };

        let workdir = match coder.new_workdir() {
            Ok(w) => w,
            Err(e) => return format!("🔨 Could not open a build directory: {e}"),
        };
        let base = proposal.base_sha.clone();
        let staged = {
            let (from, to, sha) = (checkout.clone(), workdir.clone(), base.clone());
            tokio::task::spawn_blocking(move || {
                mind_tools::code::clone_at(&from, Path::new(&to), &sha)
            })
            .await
        };
        match staged {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                let _ = std::fs::remove_dir_all(&workdir);
                return format!(
                    "🔨 Refusing to build: the checkout could not be staged at {base} — {e}\n\n\
                     A proposal whose base commit is unreachable cannot be built against the code \
                     it was reasoning about."
                );
            }
            Err(e) => return format!("🔨 Could not stage the checkout: {e}"),
        }

        // The agent's home is about to be created INSIDE this clone (the coder sets HOME here),
        // so the clone is told to ignore it before anything runs.
        let _ = mind_tools::code::exclude_paths(Path::new(&workdir), &agent_home_patterns());

        // 1. The pristine run. Everything after this depends on it having FAILED.
        let before = match self.run_check(&sandbox, &workdir, &proposal.acceptance_test).await {
            Ok(r) => r,
            Err(e) => return format!("🔨 The pristine check could not run: {e}"),
        };
        if let Some(outcome) = differential_gate(before.exit_code, before.timed_out) {
            return render_build(
                &proposal,
                before.exit_code,
                before.timed_out,
                outcome,
                None,
                "",
                &format!("{}\n{}", before.stdout, before.stderr),
                &workdir,
                0,
                // Nothing was built, so the builder has no ending to report.
                &BuilderEnd::Completed,
                "",
            );
        }

        // 2. The build.
        let task = build_task(&proposal);
        let built = match coder.run_in(&task, workdir.clone()).await {
            Ok(r) => r,
            Err(e) => return format!("🔨 The builder could not run: {e}"),
        };
        let end = builder_end(&built.summary, built.timed_out);

        // 3. The diff, against the commit it started from rather than HEAD.
        let diff = {
            let (dir, sha) = (workdir.clone(), base.clone());
            tokio::task::spawn_blocking(move || {
                mind_tools::code::staged_diff(Path::new(&dir), &sha)
            })
            .await
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or_default()
        };

        // 4. The built run — same check, same sandbox, another throwaway copy.
        let after = match self.run_check(&sandbox, &workdir, &proposal.acceptance_test).await {
            Ok(r) => r,
            Err(e) => return format!("🔨 The check on the built tree could not run: {e}"),
        };
        let outcome = built_outcome(&diff, after.exit_code, after.timed_out);
        render_build(
            &proposal,
            before.exit_code,
            before.timed_out,
            outcome,
            Some((after.exit_code, after.timed_out)),
            &diff,
            &format!("{}\n{}", after.stdout, after.stderr),
            &workdir,
            mind_tools::code::ignored_count(Path::new(&workdir)),
            &end,
            &built.summary,
        )
    }

    /// Run one acceptance test against a copy of `tree`, network-isolated and bounded.
    async fn run_check(
        &self,
        sandbox: &mind_tools::Sandbox,
        tree: &str,
        check: &str,
    ) -> std::io::Result<mind_tools::sandbox::ExecResult> {
        sandbox
            .run_seeded(
                check_limits(),
                Some(mind_tools::sandbox::SeedTree {
                    root: std::path::PathBuf::from(tree),
                    into: SEED_DIR.to_string(),
                    skip: AGENT_HOME.iter().map(|n| n.to_string()).collect(),
                }),
                vec![(CHECK_FILE.to_string(), check.to_string())],
                &format!("cd {SEED_DIR}; exec /bin/sh ../{CHECK_FILE}"),
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proposal() -> ProjectProposal {
        ProjectProposal {
            repo: "ContextCache".into(),
            goal: "Add a --stats flag to start.sh".into(),
            citations: vec!["the README".into()],
            base_sha: "a30aeb5".into(),
            acceptance_test: "./start.sh --stats | jq -e .cache_hit_rate".into(),
            why_not: "nobody asked for it".into(),
            p_merge: 0.45,
        }
    }

    /// THE rung-8 invariant. A check that already passes on the unchanged tree certifies nothing,
    /// and the pipeline must stop before the coder is spent rather than collect a meaningless
    /// green. Mutant: return `None` here and this fails by name.
    #[test]
    fn a_check_that_passes_before_the_change_stops_the_build() {
        assert_eq!(
            differential_gate(0, false),
            Some(BuildOutcome::NotADifferential)
        );
    }

    #[test]
    fn a_check_that_fails_first_licenses_a_build() {
        assert_eq!(differential_gate(1, false), None);
        assert_eq!(differential_gate(127, false), None);
    }

    /// A pristine run killed at the wall clock has not passed, so it does not stop the build.
    #[test]
    fn a_timed_out_pristine_run_is_a_failure_like_any_other() {
        assert_eq!(differential_gate(0, true), None);
        assert_eq!(differential_gate(137, true), None);
    }

    /// The third mutant: reading a timeout as success would hand a green to exactly the check that
    /// could not finish.
    #[test]
    fn a_timed_out_check_on_the_built_tree_never_verifies() {
        assert_eq!(
            built_outcome("diff --git a/x b/x\n+one\n", 0, true),
            BuildOutcome::Unverified
        );
    }

    #[test]
    fn verified_needs_a_diff_and_a_passing_check() {
        assert_eq!(
            built_outcome("diff --git a/x b/x\n+one\n", 0, false),
            BuildOutcome::Verified
        );
        assert_eq!(
            built_outcome("diff --git a/x b/x\n+one\n", 1, false),
            BuildOutcome::Unverified
        );
        assert_eq!(built_outcome("   \n", 0, false), BuildOutcome::NoChange);
    }

    #[test]
    fn diff_shape_counts_files_and_lines_not_headers() {
        let diff = "diff --git a/s.sh b/s.sh\n--- a/s.sh\n+++ b/s.sh\n@@ -1 +1,2 @@\n-old\n+new\n+more\n";
        assert_eq!(diff_shape(diff), (1, 2, 1));
    }

    /// The builder is told, in the task itself, that its own account is not what decides. That
    /// sentence is the difference between rung 8 and a self-graded exercise.
    #[test]
    fn the_builder_is_told_its_own_report_does_not_decide() {
        let task = build_task(&proposal());
        assert!(task.contains("./start.sh --stats | jq -e .cache_hit_rate"));
        assert!(task.contains("run again by something other than you"));
        assert!(task.contains("do not run `git commit`") || task.contains("Do not run `git commit`"));
    }

    #[test]
    fn the_reply_says_plainly_that_nothing_was_pushed() {
        let out = render_build(
            &proposal(),
            1,
            false,
            BuildOutcome::Verified,
            Some((0, false)),
            "diff --git a/s.sh b/s.sh\n+++ b/s.sh\n+echo hi\n",
            "ok",
            "/scratch/run-1",
            0,
            &BuilderEnd::Completed,
            "",
        );
        assert!(out.contains("VERDICT: VERIFIED"));
        assert!(out.contains("pristine tree: exit 1"));
        assert!(out.contains("built tree:    exit 0"));
        assert!(out.contains("Nothing was committed, pushed, or opened as a pull request"));
    }

    /// A stopped pipeline must SAY it stopped, rather than leaving a blank where the second run
    /// would be and letting a reader supply the missing number themselves.
    #[test]
    fn a_non_differential_reply_names_the_run_that_never_happened() {
        let out = render_build(
            &proposal(),
            0,
            false,
            BuildOutcome::NotADifferential,
            None,
            "",
            "already fine",
            "/scratch/run-1",
            0,
            &BuilderEnd::Completed,
            "",
        );
        assert!(out.contains("it should have FAILED here"));
        assert!(out.contains("built tree:    not run"));
        assert!(out.contains("NOT A DIFFERENTIAL"));
    }

    /// The structural guard, not a restatement of the gate: even a pipeline rewired to skip the
    /// stop cannot print a verdict its own first exit code contradicts. Mutant: drop the
    /// `reconcile` call in `render_build` and this fails by name.
    #[test]
    fn a_verified_claim_on_a_passing_pristine_tree_is_refused() {
        assert_eq!(
            reconcile(0, false, BuildOutcome::Verified),
            BuildOutcome::NotADifferential
        );
        let out = render_build(
            &proposal(),
            0,
            false,
            BuildOutcome::Verified,
            Some((0, false)),
            "diff --git a/x b/x\n+++ b/x\n+one\n",
            "",
            "/scratch/run-1",
            0,
            &BuilderEnd::Completed,
            "",
        );
        assert!(out.contains("NOT A DIFFERENTIAL"), "{out}");
        assert!(!out.contains("VERDICT: VERIFIED"), "{out}");
    }

    /// One list, two uses. They were nearly two hand-maintained lists — the git exclusion and the
    /// sandbox skip — which is exactly the seam that got a durable goal refused in E.HORIZON1.
    #[test]
    fn the_git_exclusion_and_the_sandbox_skip_come_from_one_list() {
        let patterns = agent_home_patterns();
        assert_eq!(patterns.len(), AGENT_HOME.len());
        for (pattern, name) in patterns.iter().zip(AGENT_HOME.iter()) {
            assert_eq!(pattern, &format!("/{name}"), "anchored at the workdir root");
        }
        assert!(AGENT_HOME.contains(&".claude"));
        assert!(AGENT_HOME.contains(&".claude.json"));
    }

    /// A reply that hides part of the tree must SAY so. The first real run reported 2,727 added
    /// lines of the agent's own session log as though they were the change it had made.
    #[test]
    fn a_reply_that_hides_paths_names_the_fact() {
        let out = render_build(
            &proposal(),
            1,
            false,
            BuildOutcome::NoChange,
            Some((1, false)),
            "",
            "",
            "/scratch/run-1",
            9,
            &BuilderEnd::Completed,
            "",
        );
        assert!(out.contains("9 path(s) are excluded"), "{out}");
        assert!(out.contains("coding agent's own home"), "{out}");
    }

    /// E.RUNG8c, from run 2. The builder had converged on a plan and was designing the change when
    /// its provider answered 429. Reporting that as "the builder left the tree as it found it"
    /// blames the builder for the lane. Mutant: stop consulting `provider_refusal` in
    /// `builder_end` and this fails by name.
    #[test]
    fn a_rate_limited_builder_is_not_reported_as_one_that_declined() {
        let end = builder_end("API Error: Request rejected (429) - Too Many Requests", false);
        assert_eq!(end, BuilderEnd::Refused("quota exhausted (429)"));
        let line = verdict_line(BuildOutcome::NoChange, &end);
        assert!(line.contains("NOT THE BUILDER'S DOING"), "{line}");
        assert!(line.contains("429"), "{line}");
        assert!(
            !line.contains("left the tree as it found it"),
            "a refused builder did not choose to leave the tree alone: {line}"
        );
    }

    /// A round killed at the clock AFTER a 429 was stopped by the 429, not by the clock.
    #[test]
    fn a_refusal_outranks_a_timeout() {
        assert_eq!(
            builder_end("usage limit reached", true),
            BuilderEnd::Refused("quota exhausted (429)")
        );
        assert_eq!(builder_end("all done", true), BuilderEnd::WallClock);
        assert_eq!(builder_end("all done", false), BuilderEnd::Completed);
    }

    /// A builder that genuinely ran and changed nothing still gets the plain sentence — the
    /// qualification must not fire on every NoChange, or it stops meaning anything.
    #[test]
    fn a_completed_builder_that_changed_nothing_is_reported_plainly() {
        let line = verdict_line(BuildOutcome::NoChange, &BuilderEnd::Completed);
        assert!(line.contains("left the tree as it found it"), "{line}");
        assert!(!line.contains("provider"), "{line}");
    }

    #[test]
    fn newest_proposal_filters_by_repo_and_ignores_junk() {
        let dir = mind_types::scratch::dir("rung8-newest");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut other = proposal();
        other.repo = "SDF Protocol".into();
        std::fs::write(
            dir.join("a.json"),
            serde_json::to_string(&proposal()).unwrap(),
        )
        .unwrap();
        std::fs::write(dir.join("b.json"), serde_json::to_string(&other).unwrap()).unwrap();
        std::fs::write(dir.join("c.json"), "{ not json").unwrap();
        std::fs::write(dir.join("d.txt"), "ignored").unwrap();

        assert_eq!(
            newest_proposal(&dir, Some("contextcache")).map(|p| p.repo),
            Some("ContextCache".to_string())
        );
        assert_eq!(newest_proposal(&dir, Some("nothing-here")), None);
        assert!(newest_proposal(&dir, None).is_some());
    }
}
