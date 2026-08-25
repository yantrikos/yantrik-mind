//! Stamps the build's git commit into the binary as `YM_BUILD_COMMIT`.
//!
//! EX4-LIVE-A persists the commit alongside every shadow decision, because candidate derivation
//! will change and an ACT-rate that cannot be attributed to a particular derivation is
//! uninterpretable two weeks later (ledger E.D4).
//!
//! This exists as a build script rather than a bare `option_env!` read at compile time because
//! cargo would not know to rebuild when HEAD moves: the stamp would silently keep reporting the
//! commit of whenever the crate last happened to compile. A provenance field that is quietly stale
//! is worse than one that says "unknown" — it looks like evidence.

use std::process::Command;

fn main() {
    // Rebuild when the checked-out commit changes. HEAD covers commits; packed-refs covers the
    // case where a ref file does not exist on its own.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/packed-refs");
    println!("cargo:rerun-if-env-changed=YM_BUILD_COMMIT");

    // An explicit override wins — a release pipeline may know the commit better than the tree does.
    if let Ok(v) = std::env::var("YM_BUILD_COMMIT") {
        if !v.trim().is_empty() {
            println!("cargo:rustc-env=YM_BUILD_COMMIT={}", v.trim());
            return;
        }
    }

    let commit = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        // Not a build failure. A tarball with no .git is a legitimate way to build this, and
        // "unknown" is an honest answer where a fabricated one would not be.
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=YM_BUILD_COMMIT={commit}");
}
