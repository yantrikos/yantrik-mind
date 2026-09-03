//! E.OBS2 — two processes, one decision log. Kill criterion 4 says this must be demonstrated with
//! REAL processes, because the whole failure is inter-process: an in-process mutex serialises
//! threads and is blind to a second `mind-core`, which is exactly how staging's chain was broken
//! on 2026-09-03. A unit test with a mocked lock would have passed against the broken code.
//!
//! The child is this same test binary re-executed with an environment variable, so no extra
//! target and no shell script is needed.

use mind_observability::{DecisionEvent, DecisionLog};

const CHILD_LOG: &str = "YM_OBS2_CHILD_LOG";
const CHILD_TEST: &str = "obs2_child_writer_role";

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "ym-obs2-{}-{}-{name}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&d).expect("temp dir");
    d.join("d.jsonl")
}

fn lines(p: &std::path::Path) -> usize {
    std::fs::read_to_string(p)
        .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0)
}

fn write_n(path: &std::path::Path, n: usize, kind: &str) {
    let log = DecisionLog::open(path);
    for i in 0..n {
        log.record(DecisionEvent::new(&format!("{kind}-{i}"), kind));
    }
}

/// THE CHILD. Inert unless the parent sets the variable, so a normal `cargo test` run does not
/// execute it as a test of its own.
#[test]
fn obs2_child_writer_role() {
    let Ok(path) = std::env::var(CHILD_LOG) else {
        return;
    };
    let path = std::path::PathBuf::from(path);
    write_n(&path, 5, "child");
    // The linger role holds its claim open long enough for the parent to kill it mid-hold.
    if std::env::var("YM_OBS2_CHILD_LINGER").is_ok() {
        std::thread::sleep(std::time::Duration::from_secs(30));
    }
    // The parent reads this to know the child really ran and really tried.
    println!("CHILD_ATTEMPTED lines_now={}", lines(&path));
}

fn spawn_child(path: &std::path::Path) -> std::process::Output {
    std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args([CHILD_TEST, "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD_LOG, path)
        .output()
        .expect("spawn child")
}

/// KILL 4: the second process must REFUSE to write, not interleave the chain.
#[test]
fn a_second_process_refuses_to_write_the_same_log() {
    let path = tmp("refuse");
    // The parent claims the log by writing to it.
    write_n(&path, 3, "parent");
    assert_eq!(lines(&path), 3, "the parent's own writes land");

    let out = spawn_child(&path);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stdout.contains("CHILD_ATTEMPTED"),
        "the child must actually have run and tried to write — otherwise this test proves \
         nothing at all.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert_eq!(
        lines(&path),
        3,
        "the child appended to a log another process holds; that is the corruption this exists \
         to prevent.\nstdout: {stdout}\nstderr: {stderr}"
    );
    // And it must SAY why, so an operator meets the cause rather than a silent absence of rows.
    assert!(
        stdout.contains("REFUSING TO WRITE") || stderr.contains("REFUSING TO WRITE"),
        "the refusal must be stated.\nstdout: {stdout}\nstderr: {stderr}"
    );
    // The chain the parent wrote is still whole.
    let log = DecisionLog::open(&path);
    assert!(log.read_all_verified().is_ok(), "the chain survived");
}

/// KILL 2: a claim dies with the process that held it. This is why the mechanism is an flock and
/// not a pid file — the kernel releases it on ANY death, including kill -9, so a crashed writer
/// cannot leave a stale claim that bricks its successor. A pid file would need a liveness check,
/// and that check is itself racy.
#[test]
fn a_killed_writers_claim_does_not_brick_the_next_one() {
    let path = tmp("stale");
    // A child takes the claim and is killed while holding it. It writes, then sleeps: the sleep is
    // what guarantees it is still alive — and still holding — when the kill arrives.
    let mut child = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args([CHILD_TEST, "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD_LOG, &path)
        .env("YM_OBS2_CHILD_LINGER", "1")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn");
    // Wait for the child's rows to appear, so the kill lands on a holder rather than a starter.
    let mut waited = 0;
    while lines(&path) == 0 && waited < 100 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        waited += 1;
    }
    assert!(lines(&path) > 0, "the child must hold the claim before it is killed");
    let _ = child.kill();
    let _ = child.wait();

    // The successor must be able to write. Under a pid file this is where it would brick.
    let before = lines(&path);
    write_n(&path, 2, "successor");
    assert_eq!(
        lines(&path),
        before + 2,
        "a dead holder's claim must not outlive it"
    );
    let log = DecisionLog::open(&path);
    assert!(log.read_all_verified().is_ok(), "and the chain is whole");
}

/// KILL 3: one writer is unaffected. The guard must not cost the ordinary case anything.
#[test]
fn a_single_writer_is_unchanged() {
    let path = tmp("single");
    write_n(&path, 50, "solo");
    assert_eq!(lines(&path), 50);
    let log = DecisionLog::open(&path);
    let events = log.read_all_verified().expect("chain verifies");
    assert_eq!(events.len(), 50, "every row landed and the chain reads");
    // A second HANDLE in the same process is not a second writer and must still work — the
    // engine opens several.
    write_n(&path, 5, "solo2");
    assert_eq!(lines(&path), 55, "a second handle in one process still writes");
    assert!(DecisionLog::open(&path).read_all_verified().is_ok());
}
