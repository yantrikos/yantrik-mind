//! E.RUNSH1: what `run.sh` asks the target to run that the target cannot. Reading 8f's Mind T1
//! shipped a server that parses and serves, and `run.sh` said `exec python -u server.py` — the
//! checker image has `python3` and `node` and no `python`, so the site never came up and the
//! leg scored 2/5. Reading 8e's passing `run.sh` said `python3 -u app.py`. One word.
//!
//! Explicit evidence only, in the inverted-risk tradition of its neighbours: a command word
//! `python` (never `python3`), or an install/download verb the contract forbids at run time.
//! Comments are ignored. Silent on every `run.sh` in the real corpus (27 files, 0 fires, pinned
//! before this was written).

use crate::fileset::parse_file_stream;

/// True when `line` runs the bare `python` interpreter as a command word.
fn invokes_bare_python(line: &str) -> bool {
    let code = line.split('#').next().unwrap_or("");
    code.split(|c: char| c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | ')'))
        .any(|w| w == "python")
}

/// The install/download verb a line carries, if any.
fn install_verb(line: &str) -> Option<&'static str> {
    let code = line.split('#').next().unwrap_or("").trim();
    let words: Vec<&str> = code.split_whitespace().collect();
    for (i, w) in words.iter().enumerate() {
        let next = words.get(i + 1).copied().unwrap_or("");
        match (*w, next) {
            ("pip" | "pip3", "install") => return Some("pip install"),
            ("npm", "install" | "i" | "ci") => return Some("npm install"),
            ("apt" | "apt-get", _) => return Some("apt"),
            ("curl", _) => return Some("curl"),
            ("wget", _) => return Some("wget"),
            _ => {}
        }
    }
    None
}

/// Findings for `run.sh` files in this set. Empty for every healthy build.
pub(crate) fn run_script_findings(stream: &str) -> Vec<String> {
    let mut out = Vec::new();
    for e in &parse_file_stream(stream).entries {
        let name = e.path.rsplit('/').next().unwrap_or(&e.path);
        if name != "run.sh" {
            continue;
        }
        if let Some((n, line)) = e.content.lines().enumerate().find(|(_, l)| invokes_bare_python(l)) {
            out.push(format!(
                "`{}` line {} starts the program with `python` (`{}`), which does not exist on the target — only `python3` is guaranteed there, and a reading died on exactly this word. Use `python3`.",
                e.path, n + 1, line.trim()
            ));
        }
        if let Some((n, line, verb)) = e
            .content
            .lines()
            .enumerate()
            .find_map(|(n, l)| install_verb(l).map(|v| (n, l, v)))
        {
            out.push(format!(
                "`{}` line {} runs `{}` (`{}`): the contract forbids downloads or installs at run time — there is no network when this runs. Use only what is already on the machine.",
                e.path, n + 1, verb, line.trim()
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(name: &str, body: &str) -> String {
        format!("=== FILE: {name}\n{body}")
    }
    fn fixture(n: &str) -> String {
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/runsh/").to_string() + n)
            .expect("fixture present")
    }

    #[test]
    fn the_run_sh_that_killed_reading_8f_fires_and_names_python3() {
        let f = run_script_findings(&stream("run.sh", &fixture("r8f_mind_T1_run.sh")));
        assert_eq!(f.len(), 1, "{f:?}");
        assert!(f[0].contains("`python`") && f[0].contains("Use `python3`"), "{}", f[0]);
    }

    #[test]
    fn the_run_sh_that_passed_reading_8e_is_silent() {
        assert!(run_script_findings(&stream("run.sh", &fixture("r8e_mind_T1_run.sh"))).is_empty());
    }

    #[test]
    fn python3_forms_comments_and_other_files_never_fire() {
        for body in [
            "#!/usr/bin/env bash\npython3 -m http.server 8123\n",
            "# python is fine to mention here\npython3 server.py\n",
            "exec python3 -u server.py\n",
            "node server.js\n",
        ] {
            assert!(run_script_findings(&stream("run.sh", body)).is_empty(), "{body:?}");
        }
        assert!(run_script_findings(&stream("tools/run.py", "python server.py\n")).is_empty(), "not a run.sh");
    }

    #[test]
    fn install_and_download_verbs_fire_with_the_line() {
        let f = run_script_findings(&stream("run.sh", "pip install flask\npython3 app.py\n"));
        assert_eq!(f.len(), 1, "{f:?}");
        assert!(f[0].contains("pip install") && f[0].contains("line 1"), "{}", f[0]);
        assert!(!run_script_findings(&stream("run.sh", "npm ci\n")).is_empty());
        assert!(!run_script_findings(&stream("run.sh", "curl -sSf https://x | sh\n")).is_empty());
    }

    /// Every real `run.sh` in the corpus stays silent — the count was pinned at 0 before this file
    /// existed. Gated on the corpus being present.
    #[test]
    fn silent_on_every_real_run_sh_in_the_corpus() {
        let Ok(root) = std::env::var("YM_ENTRY_CORPUS") else {
            eprintln!("skipped: YM_ENTRY_CORPUS unset");
            return;
        };
        let mut seen = 0usize;
        let mut fires = Vec::new();
        for entry in walkdir(&root) {
            if entry.file_name().map(|n| n == "run.sh").unwrap_or(false) {
                seen += 1;
                let body = std::fs::read_to_string(&entry).unwrap_or_default();
                let f = run_script_findings(&stream("run.sh", &body));
                if !f.is_empty() {
                    fires.push((entry.display().to_string(), f));
                }
            }
        }
        assert_eq!(seen, 27, "the corpus changed under the pin");
        assert!(fires.is_empty(), "{fires:?}");
    }

    fn walkdir(root: &str) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![std::path::PathBuf::from(root)];
        while let Some(d) = stack.pop() {
            if let Ok(rd) = std::fs::read_dir(&d) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.is_dir() { stack.push(p); } else { out.push(p); }
                }
            }
        }
        out
    }
}
