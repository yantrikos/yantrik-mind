//! scratch — a temporary path for a test that is unique per RUN and removed when the test ends.
//!
//! Test support, living here because every crate already depends on `mind-types` and a scratch path
//! is not worth a crate of its own.
//!
//! ## Why this exists
//!
//! Seventy-three sites built their scratch path as `temp_dir().join(format!("x_{}",
//! process::id()))` — keyed on the PID ALONE, and never removed. Two things follow, and both have
//! actually happened:
//!
//!   * **Leakage.** Over a thousand stale directories had accumulated in `%TEMP%` — 185 each for
//!     eight `ym_devtrust_*` names, 157 `ym_seal_test_*`, 116 `ym_conv_p1_*`, 90 `ym_p2d_*`.
//!   * **Collision.** PIDs are recycled. `surface.rs`'s orders test read a PREVIOUS run's two
//!     sleeping orders alongside its own and counted four. It had passed for as long as it existed
//!     and failed the moment an unrelated test shifted which PID it got — so it was never right,
//!     only lucky.
//!
//! A path that is unique per run cannot collide, and one that removes itself cannot leak. The
//! cleanup runs through `Drop`, so a FAILING assertion still cleans up: a test that leaves state
//! behind on the failure path poisons the next run precisely when someone is trying to debug.

use std::path::{Path, PathBuf};

/// A scratch path that removes itself when it goes out of scope.
///
/// Hold it in a binding for the life of the test — `let scratch = scratch::dir("mytest");` — and
/// pass `scratch.path()` around. Dropping it early removes the files early, which is why it must
/// not be bound to `_`.
pub struct Scratch {
    path: PathBuf,
}

impl Scratch {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The path as a string, for the APIs here that take one.
    ///
    /// NOT called `display`: `Path::display` already exists and is reached through `Deref`, and a
    /// method that shadows a std one with different semantics is a trap. `join` is not defined here
    /// for the same reason — `Path::join` takes `impl AsRef<Path>` and a narrower `&str` copy of it
    /// silently rejects a `String` at the call site.
    pub fn as_str(&self) -> std::borrow::Cow<'_, str> {
        self.path.to_string_lossy()
    }
}

/// So a `Scratch` can be passed anywhere a `&Path` is wanted — `open(&scratch)` — and can call
/// `Path`'s own methods. Without this, adopting the helper would mean touching every call site
/// rather than the one line that builds the path.
impl std::ops::Deref for Scratch {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for Scratch {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

/// For the APIs that take `impl Into<PathBuf>` — `Into<PathBuf>` is reached through `AsRef<OsStr>`,
/// not `AsRef<Path>`, so both are needed for a `Scratch` to be a drop-in.
impl AsRef<std::ffi::OsStr> for Scratch {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.path.as_os_str()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Silent by design: a cleanup failure must never turn a passing test red, and must never
        // mask the real assertion in a failing one.
        //
        // RETRIED, because the first version of this did not work and measuring caught it. Windows
        // refuses to remove a directory while any handle inside it is open, and a sqlite connection
        // closed microseconds earlier may not have released yet — so the removal failed, the error
        // was swallowed, and the directories accumulated exactly as before. A few short retries is
        // the difference between a helper that cleans up and one that only claims to.
        for attempt in 0..5 {
            let gone = if self.path.is_dir() {
                std::fs::remove_dir_all(&self.path).is_ok()
            } else {
                std::fs::remove_file(&self.path).is_ok()
            } || !self.path.exists();
            if gone {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20 * (attempt + 1)));
        }
        self.sweep_siblings();
    }
}

impl Scratch {
    /// Remove anything a library wrote ALONGSIDE this path.
    ///
    /// A sqlite database spawns `-wal` and `-shm`; this codebase also parks a
    /// `<db>.read_receipts.jsonl` and a `<db>.decisions.jsonl` next to it. Removing only the exact
    /// path leaves those behind — 292 `ym_snap_live*` entries were mostly sidecars. Safe because
    /// the stem is unique to this scratch: nothing else can share the prefix.
    fn sweep_siblings(&self) {
        // Only for FILE scratches. A directory's contents are already gone with it, and a directory
        // name has no extension to separate it from a sibling whose counter merely starts the same.
        if self.path.is_dir() {
            return;
        }
        let (Some(parent), Some(stem)) = (self.path.parent(), self.path.file_name()) else { return };
        let Some(stem) = stem.to_str() else { return };
        let Ok(entries) = std::fs::read_dir(parent) else { return };
        for e in entries.flatten() {
            let name = e.file_name();
            let Some(name) = name.to_str() else { continue };
            // The suffix must begin with a SEPARATOR. `starts_with(stem)` alone is wrong: two
            // scratches created in the same nanosecond differ only by their trailing counter, so
            // `ym_x_1_2_1` is a prefix of `ym_x_1_2_10` — and dropping the first would delete the
            // second while it was still in use. A sidecar is always `<name>.something` or
            // `<name>-something` (`-wal`, `-shm`, `.read_receipts.jsonl`); a sibling scratch never is.
            let sidecar = name.strip_prefix(stem).is_some_and(|rest| rest.starts_with(['.', '-']));
            if sidecar {
                let p = e.path();
                let _ = if p.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
            }
        }
    }
}

/// A unique suffix: the pid, plus a nanosecond stamp, plus a counter.
///
/// The pid alone is not unique — it is recycled, and two test binaries in the same `cargo test`
/// run can hold it in sequence. The stamp alone is not unique either: two threads can read the
/// same nanosecond. The counter closes that, so this is unique within a run and across runs.
fn unique(tag: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("ym_{tag}_{}_{nanos}_{seq}", std::process::id())
}

/// A fresh, empty directory that is removed when the returned value drops.
pub fn dir(tag: &str) -> Scratch {
    let path = std::env::temp_dir().join(unique(tag));
    let _ = std::fs::create_dir_all(&path);
    Scratch { path }
}

/// A fresh file path — NOT created, just named — removed when the returned value drops.
///
/// `ext` is appended as given: `file("log", "jsonl")` names `…/ym_log_123_456_0.jsonl`.
pub fn file(tag: &str, ext: &str) -> Scratch {
    let mut name = unique(tag);
    if !ext.is_empty() {
        name.push('.');
        name.push_str(ext);
    }
    Scratch { path: std::env::temp_dir().join(name) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_scratches_never_share_a_path() {
        // The property the pid alone did not have. Within one process the pid is CONSTANT, so if
        // uniqueness came from it these would be equal.
        let a = dir("collide");
        let b = dir("collide");
        assert_ne!(a.path(), b.path(), "two scratch dirs in one process must differ");
        assert!(a.path().to_string_lossy().contains("collide"));

        // And across many, including from several threads at once — one nanosecond clock read can
        // serve two threads, which is what the counter is for.
        let paths: Vec<String> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..8)
                .map(|_| s.spawn(|| (0..25).map(|_| dir("race").as_str().to_string()).collect::<Vec<_>>()))
                .collect();
            handles.into_iter().flat_map(|h| h.join().unwrap()).collect()
        });
        let unique: std::collections::HashSet<&String> = paths.iter().collect();
        assert_eq!(unique.len(), paths.len(), "200 scratch paths across 8 threads must all differ");
    }

    #[test]
    fn a_scratch_removes_itself_including_its_contents() {
        let path = {
            let s = dir("cleanup");
            std::fs::write(s.join("inner.txt"), b"x").unwrap();
            assert!(s.join("inner.txt").exists());
            s.path().to_path_buf()
        };
        assert!(!path.exists(), "the directory and everything in it goes when the guard drops");
    }

    #[test]
    fn cleanup_survives_a_panic_because_it_runs_on_unwind() {
        // The case that matters: a test that FAILS must still not poison the next run. This is the
        // reason cleanup is a Drop and not a line at the end of the test.
        let seen = std::sync::Arc::new(std::sync::Mutex::new(PathBuf::new()));
        let probe = seen.clone();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let s = file("panicky", "txt");
            std::fs::write(s.path(), b"x").unwrap();
            *probe.lock().unwrap() = s.path().to_path_buf();
            panic!("as a failing assertion would");
        }));
        let path = seen.lock().unwrap().clone();
        assert!(!path.as_os_str().is_empty(), "the probe recorded a path");
        assert!(!path.exists(), "a panicking test still cleans up after itself");
    }

    #[test]
    fn sidecars_written_next_to_a_scratch_file_go_with_it() {
        // A sqlite db spawns `-wal`/`-shm`, and this codebase parks `<db>.read_receipts.jsonl`
        // beside it. Removing only the exact path left those behind, which is most of what was
        // still accumulating after the first version of this helper.
        let (main, wal, receipts) = {
            let s = file("sidecar", "db");
            let main = s.path().to_path_buf();
            let wal = PathBuf::from(format!("{}-wal", main.display()));
            let receipts = PathBuf::from(format!("{}.read_receipts.jsonl", main.display()));
            std::fs::write(&main, b"x").unwrap();
            std::fs::write(&wal, b"x").unwrap();
            std::fs::write(&receipts, b"x").unwrap();
            (main, wal, receipts)
        };
        assert!(!main.exists(), "the file itself");
        assert!(!wal.exists(), "the sqlite write-ahead log");
        assert!(!receipts.exists(), "and the receipts ledger parked beside it");
    }

    #[test]
    fn the_sidecar_sweep_cannot_delete_a_neighbouring_scratch() {
        // The defect this guard exists for, and it was mine: two scratches made in the same
        // nanosecond differ only by the trailing counter, so `ym_x_1_2_1` is a PREFIX of
        // `ym_x_1_2_10`. A sweep keyed on `starts_with` alone would delete a live neighbour.
        let a = dir("neighbour");
        let sibling = PathBuf::from(format!("{}0", a.path().display()));
        std::fs::create_dir_all(&sibling).unwrap();
        std::fs::write(sibling.join("keep.txt"), b"x").unwrap();
        drop(a);
        assert!(sibling.join("keep.txt").exists(), "a neighbour whose name merely extends ours must survive");
        let _ = std::fs::remove_dir_all(&sibling);
    }

    #[test]
    fn a_file_scratch_is_named_but_not_created() {
        let s = file("named", "jsonl");
        assert!(s.as_str().ends_with(".jsonl"));
        assert!(!s.path().exists(), "naming a path must not create it");
    }
}
