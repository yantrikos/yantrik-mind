//! The coverage router's labelled corpus and its pre-registered bar (ARCH-6 P.3, ledger E.PK3).
//!
//! The router (`mind_spec::coverage`) decides which pack a query calls for from the publishers'
//! coverage phrases alone. This module holds the twelve library packs' REAL coverage lists (copied
//! verbatim from their manifests), a hand-labelled query set written from those lists and the packs'
//! descriptions BEFORE the router ran on anything, and the bar the router must clear:
//!
//!   * pack-applies queries: top-1 agreement ≥ 0.80 (a tie counts as wrong);
//!   * no-pack queries:      abstention ≥ 0.90 (a tie counts as right).
//!
//! The labels are NOT independent of the coverage authors (same lineage), so this bar measures
//! whether the router reads coverage as its authors meant it — not whether coverage is what users
//! need. The gated test (`YM_COVERAGE_P3=1`) prints every query's top two matches and the phrase
//! that earned each, so a KILL says which: the policy, or the coverage lists.
//!
//! Eval custody applies (`deploy/self_improve.sh` blocks self-authored changes here).

use mind_types::MemoryFacade;

/// One library pack: its real id parts and its coverage phrases, verbatim from the manifest.
pub struct LibraryPack {
    pub name: &'static str,
    pub version: &'static str,
    pub coverage: &'static [&'static str],
}

impl LibraryPack {
    pub fn pack_id(&self) -> String {
        format!("yantrik/{}@{}", self.name, self.version)
    }
}

/// The twelve packs the box's library carries (`packs/dist/*.ydbpack`, all 64-dim potion-base-2M).
pub const LIBRARY: &[LibraryPack] = &[
    LibraryPack {
        name: "web-craft",
        version: "0.3.1",
        coverage: &[
            "web-craft writing a complete self-contained HTML page with no network dependencies",
            "web-craft colour tokens and supporting light and dark themes correctly",
            "web-craft type scale, line length and typographic hierarchy",
            "web-craft spacing systems and responsive layout with grid and flex",
            "web-craft overflow: letting wide tables and code scroll without the page scrolling sideways",
            "web-craft hover, focus and motion states",
            "web-craft inline SVG and CSS gradients instead of photographs",
            "web-craft what never to put on a page: lorem ipsum, invented facts, the generic AI look",
        ],
    },
    LibraryPack {
        name: "mcp-spec",
        version: "0.3.2",
        coverage: &[
            "MCP protocol revision 2026-07-28 changes and migration from earlier revisions",
            "MCP stateless requests, _meta fields and capability negotiation",
            "MCP tools: tools/list, tools/call, schemas, annotations and x-mcp-header",
            "MCP resources, resource templates, prompts and content blocks",
            "MCP Multi Round-Trip Requests, elicitation, sampling and roots",
            "MCP Streamable HTTP and stdio transports, headers and SSE streams",
            "MCP OAuth authorization, Client ID Metadata Documents and token validation",
            "MCP subscriptions, notifications, server discovery and caching",
            "MCP pagination, completion, logging and progress utilities",
            "MCP extensions: tasks, apps, and extension negotiation",
            "MCP error codes, JSON-RPC message rules and JSON Schema usage",
        ],
    },
    LibraryPack {
        name: "java-stdlib",
        version: "0.2.0",
        coverage: &[
            "Java standard library class and method signatures",
            "Java collections, streams and optionals",
            "Java file, path and process operations",
            "Java HTTP client, URI and networking",
            "Java time, dates, formatting and duration",
            "Java concurrency, executors, futures and virtual threads",
        ],
    },
    LibraryPack {
        name: "java-modern",
        version: "0.1.0",
        coverage: &[
            "Modern Java language features: records, sealed types, pattern matching, text blocks",
            "Java virtual threads and structured concurrency",
            "Java concurrency correctness: visibility, atomicity, thread-safe collections",
            "Java equality, hashCode, comparison and collection contracts",
            "Java exception handling, try-with-resources and interruption",
            "java.time, BigDecimal and numeric precision",
            "Java streams and the Collections API",
            "Java security defaults: SQL injection, XML parsing, deserialization, randomness",
        ],
    },
    LibraryPack {
        name: "c-safety",
        version: "0.1.0",
        coverage: &[
            "C memory safety: allocation, lifetime, use-after-free, double free",
            "C undefined behaviour and what optimisers do with it",
            "C string handling and buffer overflows",
            "C integer overflow, promotion and conversion rules",
            "C error handling, cleanup patterns and resource management",
            "C compiler flags, sanitizers and static analysis",
        ],
    },
    LibraryPack {
        name: "php-modern",
        version: "0.1.0",
        coverage: &[
            "Modern PHP 8 language features: enums, readonly, promotion, match, attributes",
            "PHP type system, strict_types and nullability",
            "PHP arrays, comparison semantics and string handling",
            "PHP security: SQL injection, XSS escaping, deserialization, file uploads",
            "PHP passwords, sessions, cryptographic randomness",
            "PHP error handling, exceptions and JSON",
            "PHP dates and decimal arithmetic",
        ],
    },
    LibraryPack {
        name: "python-stdlib",
        version: "0.1.0",
        coverage: &[
            "Python standard library function and class signatures",
            "Python file, path and subprocess operations",
            "Python text, JSON, CSV and regular expression handling",
            "Python dates, times, collections and itertools",
            "Python logging, testing mocks and concurrency futures",
        ],
    },
    LibraryPack {
        name: "uk-statutory-rates",
        version: "0.1.0",
        coverage: &[
            "UK National Minimum Wage and National Living Wage hourly rates by age band",
            "UK apprentice minimum wage rate and who is actually entitled to it",
            "when UK minimum wage rates change and how age band boundaries work",
            "UK Statutory Sick Pay weekly rate, the 80% of earnings comparison and the 8-week averaging",
            "UK Statutory Sick Pay duration, qualifying days, waiting days and ineligibility",
            "England and Wales tenancy deposit protection deadlines and approved schemes",
            "what information a landlord must give a tenant about a protected deposit",
            "which tenancies and which UK jurisdictions the deposit rules cover",
        ],
    },
    LibraryPack {
        name: "letterpress",
        version: "0.1.0",
        coverage: &[
            "letterpress op language: SITE, THEME, SECTION, TEXT, ACTION, MEDIA, ITEM",
            "letterpress section kinds hero, features, cta, proof, detail, faq, roster, note, quote, gallery",
            "letterpress planning a page: which sections a brief needs and in what order",
            "letterpress choosing a typographic family and a single accent colour",
            "letterpress choosing a motif drawing for a subject",
            "letterpress writing copy: emphasis brackets, ledes, item bodies, FAQ answers",
            "letterpress what never to invent: prices, clock times, benchmarks, testimonials",
            "letterpress common errors and what the compiler rejects",
        ],
    },
    LibraryPack {
        name: "game-feel-craft",
        version: "0.1.0",
        coverage: &[
            "tuning the feel of a 2D platformer or action game",
            "choosing jump height, hang time, gravity and impulse",
            "coyote time, input buffering and jump forgiveness",
            "variable jump height and short hops",
            "hitstop, screenshake, knockback and impact feedback",
            "invulnerability frames, mercy windows and damage response",
            "ground acceleration, deceleration, turnaround and traction",
            "camera lookahead, smoothing and deadzone for a side-on game",
            "choosing player, hazard, background and UI colours for a game",
            "fail states, checkpoints, respawn delay and death penalties",
            "accessibility settings for motion, shake and flashing in games",
            "converting tuning values between frames and milliseconds",
            "writing a tuning config for Godot, Unity or Phaser",
            "reviewing a game-feel spec, or why generated tuning values are unusable",
        ],
    },
    LibraryPack {
        name: "react-craft",
        version: "0.2.1",
        coverage: &[
            "writing React components and hooks",
            "React 19 Actions, useActionState, useOptimistic, useFormStatus and use()",
            "React Server Components and the 'use client' boundary",
            "useEffect discipline, cleanup, StrictMode and fetch race conditions",
            "React list keys, reconciliation and component remounting",
            "React context, memoisation and render performance",
            "React forms, controlled inputs and accessibility",
            "React error boundaries, Suspense and concurrent rendering",
        ],
    },
    LibraryPack {
        name: "wordpress-theme",
        version: "0.2.1",
        coverage: &[
            "WordPress block theme structure and required files",
            "theme.json settings, styles and generated CSS custom properties",
            "WordPress block markup in templates and template parts",
            "WordPress block patterns and style variations",
            "WordPress theme functions.php, enqueueing and theme supports",
            "CSS for WordPress themes: layout, alignment, specificity",
            "Modern CSS: fluid typography, grid, container queries, cascade layers",
            "WordPress child themes and classic theme structure",
            "premium hero, magazine layout, pullquote and stats sections",
            "WordPress core component blocks: gallery, media-text, columns, search, social links",
        ],
    },
];

/// One labelled query. `accept` names every pack a lease may land on and still be right (two packs
/// genuinely cover java.time); empty means the router must abstain.
pub struct RouteCase {
    pub id: &'static str,
    pub query: &'static str,
    pub accept: &'static [&'static str],
    pub note: &'static str,
}

const WEB: &str = "yantrik/web-craft@0.3.1";
const MCP: &str = "yantrik/mcp-spec@0.3.2";
const JSTD: &str = "yantrik/java-stdlib@0.2.0";
const JMOD: &str = "yantrik/java-modern@0.1.0";
const CSAFE: &str = "yantrik/c-safety@0.1.0";
const PHP: &str = "yantrik/php-modern@0.1.0";
const PY: &str = "yantrik/python-stdlib@0.1.0";
const UK: &str = "yantrik/uk-statutory-rates@0.1.0";
const LP: &str = "yantrik/letterpress@0.1.0";
const GAME: &str = "yantrik/game-feel-craft@0.1.0";
const REACT: &str = "yantrik/react-craft@0.2.1";
const WP: &str = "yantrik/wordpress-theme@0.2.1";

fn c(id: &'static str, query: &'static str, accept: &'static [&'static str], note: &'static str) -> RouteCase {
    RouteCase { id, query, accept, note }
}

/// The frozen corpus. Written from the coverage lists and descriptions, then not touched to fit
/// the router. The last three pack cases are the live probes from E.PK1/E.PK2, verbatim.
pub fn cases() -> Vec<RouteCase> {
    vec![
        // ── pack applies ────────────────────────────────────────────────────────────────────
        c("web-type", "how do I set up a type scale and line length for body text on a landing page", &[WEB], ""),
        c("web-overflow", "the table is wider than the phone screen and the whole page scrolls sideways, how should it behave", &[WEB], ""),
        c("web-ai-look", "what makes a generated-looking page look generic and how do I avoid the AI look", &[WEB], ""),
        c("mcp-tools", "how does an MCP client call tools/list and tools/call and what do the annotations mean", &[MCP], ""),
        c("mcp-oauth", "MCP OAuth authorization with client id metadata documents and token validation", &[MCP], ""),
        c("mcp-migrate", "what changed in the MCP protocol revision 2026-07-28 and how do I migrate", &[MCP], ""),
        c("jstd-files", "what is the signature of Files.readString and how do I read a path in Java", &[JSTD], ""),
        c("jstd-http", "Java HttpClient send a request and read the response body", &[JSTD], ""),
        c("jstd-time", "format a java.time.LocalDate and compute a Duration between two instants", &[JSTD, JMOD], "both packs cover java.time"),
        c("jmod-records", "when should I use records and sealed interfaces with pattern matching in Java", &[JMOD], ""),
        c("jmod-vthreads", "virtual threads and structured concurrency in Java", &[JMOD, JSTD], "both packs cover virtual threads"),
        c("jmod-equals", "how do equals and hashCode contracts interact with HashSet in Java", &[JMOD], ""),
        c("c-uaf", "use-after-free and double free in C, how do I reason about object lifetime", &[CSAFE], ""),
        c("c-flags", "which C compiler flags and sanitizers catch undefined behaviour", &[CSAFE], ""),
        c("c-strncpy", "strncpy and buffer overflow when copying strings in C", &[CSAFE], ""),
        c("php-enums", "PHP 8 enums, readonly properties and constructor promotion", &[PHP], ""),
        c("php-xss", "escaping output to prevent XSS and using prepared statements in PHP", &[PHP], ""),
        c("php-passwords", "password_hash and secure session handling in PHP", &[PHP], ""),
        c("py-subprocess", "run a subprocess in Python and capture its output", &[PY], ""),
        c("py-csv-json", "parse a CSV file and a JSON document with the Python standard library", &[PY], ""),
        c("py-logging", "Python logging configuration and unittest mock patch", &[PY], ""),
        c("uk-nmw", "what is the UK national minimum wage for a 20 year old this year", &[UK], ""),
        c("uk-ssp", "statutory sick pay weekly rate and waiting days in the UK", &[UK], ""),
        c("uk-deposit", "how long does a landlord in England have to protect a tenancy deposit", &[UK], ""),
        c("lp-sections", "which letterpress section kinds should a product launch page use and in what order", &[LP], ""),
        c("lp-ops", "letterpress SITE THEME SECTION ops and what the compiler rejects", &[LP], ""),
        c("game-jump", "tune jump height, gravity and coyote time for a 2D platformer", &[GAME], ""),
        c("game-hitstop", "hitstop and screenshake values for impact feedback in an action game", &[GAME], ""),
        c("game-camera", "camera lookahead and deadzone for a side-scrolling game", &[GAME], ""),
        c("react-effect", "useEffect cleanup and fetch race conditions in React", &[REACT], ""),
        c("react-rsc", "React server components and where the use client boundary goes", &[REACT], ""),
        c("react-actions", "React 19 useActionState and useOptimistic for a form", &[REACT], ""),
        c("wp-themejson", "theme.json settings and styles for a WordPress block theme", &[WP], ""),
        c("wp-enqueue", "how to enqueue styles in functions.php and add theme supports in WordPress", &[WP], ""),
        c("wp-magazine", "build a magazine layout with pullquote and stats sections in a WordPress block theme", &[WP], ""),
        c("live-plain", "which default looks make a page read as machine-made", &[WEB], "E.PK1 live probe A: borderline at the corpus floor"),
        c("live-table", "letting a wide table scroll without the whole page scrolling sideways", &[WEB], "E.PK1 live probe B: corpus surfaced neighbours"),
        c("live-verbatim", "the default looks that read as machine-made: a centered hero over a purple-to-pink gradient, six identical icon cards in a row, emoji as bullet points — what is the minimum repair?", &[WEB], "E.PK2 live turn"),
        // ── no pack applies: the router must abstain ───────────────────────────────────────
        c("no-arith", "what is seventeen multiplied by twenty three", &[], ""),
        c("no-reminder", "remind me to call the plumber on tuesday at 9", &[], ""),
        c("no-calendar", "what is on my calendar tomorrow afternoon", &[], ""),
        c("no-mail", "did any new mail arrive from the school today", &[], ""),
        c("no-weather", "how is the weather looking for the weekend", &[], ""),
        c("no-decision", "what did we decide about the birthday plan last week", &[], ""),
        c("no-news", "summarise the news on interest rates this morning", &[], ""),
        c("no-table", "book a table for four at an italian place on friday", &[], ""),
        c("no-festival", "how many days until rath yatra", &[], ""),
        c("no-translate", "translate good morning into bengali", &[], ""),
        c("no-java-coffee", "the java on my kitchen counter went cold, should I reheat coffee in the microwave", &[], "deceptive: shares a word with two packs"),
        c("no-python-snake", "our python got out of its tank again, how do I keep a pet snake warm", &[], "deceptive: shares a word with a pack"),
    ]
}

/// Seal the library packs into `dir` (one placeholder row each; the manifests carry the real
/// coverage) and return their ids. Requires `mind-memory`'s `fixtures` feature.
#[cfg(any(test, feature = "fixtures"))]
pub fn seal_library(dir: &std::path::Path) -> Result<Vec<String>, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut ids = Vec::new();
    for p in LIBRARY {
        let dest = dir.join(format!("{}-{}.ydbpack", p.name, p.version));
        let ns = p.name.replace('-', "_");
        let id = mind_memory::fixtures::seal_fixture_pack_full(
            dest.to_str().unwrap(),
            "yantrik",
            p.name,
            p.version,
            &ns,
            &[&format!("{} — one placeholder row; the coverage list is the fixture.", p.name)],
            Some(p.coverage),
            None,
            None,
        )?;
        ids.push(id);
    }
    Ok(ids)
}

/// The corpus scored against the bar, with one line per query for the record.
#[derive(Debug, Default)]
pub struct RouterScore {
    pub pack_total: usize,
    pub pack_agree: usize,
    pub nopack_total: usize,
    pub nopack_abstain: usize,
    pub lines: Vec<String>,
}

impl RouterScore {
    pub fn agreement(&self) -> f64 {
        if self.pack_total == 0 { 0.0 } else { self.pack_agree as f64 / self.pack_total as f64 }
    }
    pub fn abstention(&self) -> f64 {
        if self.nopack_total == 0 { 0.0 } else { self.nopack_abstain as f64 / self.nopack_total as f64 }
    }
    /// The pre-registered bar (E.PK3).
    pub fn bar_met(&self) -> bool {
        self.agreement() >= 0.80 && self.abstention() >= 0.90
    }
    pub fn render(&self) -> String {
        let mut out = String::new();
        for l in &self.lines {
            out.push_str(l);
            out.push('\n');
        }
        out.push_str(&format!(
            "P3 ROUTER: agreement {}/{} ({:.2}) {} · abstention {}/{} ({:.2}) {} · bar {}\n",
            self.pack_agree,
            self.pack_total,
            self.agreement(),
            if self.agreement() >= 0.80 { "GREEN" } else { "RED" },
            self.nopack_abstain,
            self.nopack_total,
            self.abstention(),
            if self.abstention() >= 0.90 { "GREEN" } else { "RED" },
            if self.bar_met() { "MET" } else { "NOT MET" }
        ));
        out
    }
}

/// Route every case through a memory whose catalog holds the library, and score it.
pub async fn run_router_oracle(mem: &dyn MemoryFacade) -> RouterScore {
    let mut score = RouterScore::default();
    for case in cases() {
        let (ranked, route) = match mem.route_packs(case.query).await {
            Ok(x) => x,
            Err(e) => {
                score.lines.push(format!("{}: ERROR {e}", case.id));
                if case.accept.is_empty() { score.nopack_total += 1 } else { score.pack_total += 1 }
                continue;
            }
        };
        let top = ranked.first().map(|m| format!("{}@{:.2} ← “{}”", m.pack_id, m.sim, m.phrase.chars().take(44).collect::<String>())).unwrap_or_else(|| "—".into());
        let second = ranked.get(1).map(|m| format!("{}@{:.2}", m.pack_id, m.sim)).unwrap_or_else(|| "—".into());
        let leased = route.leased().map(str::to_string);
        let ok = if case.accept.is_empty() {
            score.nopack_total += 1;
            let ok = leased.is_none();
            if ok { score.nopack_abstain += 1 }
            ok
        } else {
            score.pack_total += 1;
            let ok = leased.as_deref().is_some_and(|l| case.accept.contains(&l));
            if ok { score.pack_agree += 1 }
            ok
        };
        score.lines.push(format!(
            "  {} {:<16} {:<20} top {top} · 2nd {second}{}",
            if ok { "OK  " } else { "MISS" },
            case.id,
            route.label(),
            if case.note.is_empty() { String::new() } else { format!(" · {}", case.note) }
        ));
    }
    score
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("ym_p3_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    /// The corpus itself is well-formed before anything is routed: sizes the pre-registration
    /// promised, every accepted pack in the library, ids unique.
    #[test]
    fn the_corpus_is_the_one_that_was_pre_registered() {
        let cs = cases();
        let pack: Vec<&RouteCase> = cs.iter().filter(|c| !c.accept.is_empty()).collect();
        let nopack: Vec<&RouteCase> = cs.iter().filter(|c| c.accept.is_empty()).collect();
        assert!(pack.len() >= 30, "pack cases: {}", pack.len());
        assert!(nopack.len() >= 10, "no-pack cases: {}", nopack.len());
        let ids: std::collections::HashSet<&str> = cs.iter().map(|c| c.id).collect();
        assert_eq!(ids.len(), cs.len(), "duplicate case ids");
        let lib: Vec<String> = LIBRARY.iter().map(|p| p.pack_id()).collect();
        for c in &pack {
            for a in c.accept {
                assert!(lib.iter().any(|l| l == a), "{}: accepts unknown pack {a}", c.id);
            }
        }
        assert_eq!(LIBRARY.len(), 12);
        assert!(LIBRARY.iter().all(|p| !p.coverage.is_empty()));
    }

    /// Mechanics only (the merge gate must not hinge on a calibration question): the library
    /// seals, the catalog lists all twelve unmounted, routing returns a full ranking with a named
    /// verdict, and an empty catalog abstains with NoPacks.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_router_ranks_the_whole_library_without_mounting_anything() {
        use mind_types::memory::{AbstainReason, PackRoute};
        let dir = scratch("smoke");
        let ids = seal_library(&dir).unwrap();
        assert_eq!(ids.len(), 12);
        let mem = mind_memory::MemoryHandle::spawn(":memory:", 64).unwrap();
        let (_, empty) = mem.route_packs("anything").await.unwrap();
        assert_eq!(empty, PackRoute::Abstain { reason: AbstainReason::NoPacks, best: None });
        mem.set_pack_library(dir.to_str().unwrap()).await.unwrap();
        let cat = mem.available_packs().await.unwrap();
        assert_eq!(cat.len(), 12, "{cat:?}");
        assert!(cat.iter().all(|e| !e.mounted));
        let (ranked, route) = mem.route_packs("coyote time and jump buffering for a platformer").await.unwrap();
        assert_eq!(ranked.len(), 12);
        assert!(["lease", "abstain:below_floor", "abstain:tie"].contains(&route.label()));
        assert!(mem.mounted_packs().await.unwrap().is_empty(), "routing mounted something");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// THE BAR (E.PK3). Gated like the other oracles: `YM_COVERAGE_P3=1`. Prints every query's
    /// top two matches with the phrase that earned each, so a RED says which is wrong — the policy
    /// or a coverage list.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn coverage_router_meets_its_preregistered_bar() {
        if std::env::var("YM_COVERAGE_P3").as_deref() != Ok("1") {
            println!("COVERAGE-ORACLE: gated (set YM_COVERAGE_P3=1)");
            return;
        }
        let dir = scratch("bar");
        seal_library(&dir).unwrap();
        let mem = mind_memory::MemoryHandle::spawn(":memory:", 64).unwrap();
        mem.set_pack_library(dir.to_str().unwrap()).await.unwrap();
        let score = run_router_oracle(&mem).await;
        println!("{}", score.render());
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            score.bar_met(),
            "E.PK3 bar not met: agreement {:.2} (>= 0.80), abstention {:.2} (>= 0.90) — see the per-query lines above",
            score.agreement(),
            score.abstention()
        );
    }

    /// E.PK4 wall (1): ATTACH-HARM CONTROL on the REAL packs. Gated: `YM_PACK_DIST=<dir>` of
    /// `.ydbpack` files. With every mountable pack in the directory mounted, the corpus's no-pack
    /// queries must surface ZERO rows through `recall_from_packs` — any row is KILL for that pack's
    /// floor (the publisher sweeps it) and blocks leasing it by default. Prints every pack's mount
    /// result and every query's row count, so a KILL names the pack and the query.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn attach_harm_control_on_the_real_packs() {
        use mind_types::MemoryFacade;
        let Ok(dir) = std::env::var("YM_PACK_DIST") else {
            println!("ATTACH-HARM CONTROL: gated (set YM_PACK_DIST=<dir of .ydbpack files>)");
            return;
        };
        let mem = mind_memory::MemoryHandle::spawn(":memory:", 64).unwrap();
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("{dir}: {e}"))
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map_or(false, |x| x == "ydbpack"))
            .collect();
        files.sort();
        let (mut mounted, mut unmountable) = (Vec::new(), Vec::new());
        for f in &files {
            match mem.mount_pack(f.to_str().unwrap()).await {
                Ok(id) => mounted.push(id),
                Err(e) => unmountable.push(format!("{}: {e}", f.file_name().unwrap().to_string_lossy())),
            }
        }
        println!("ATTACH-HARM CONTROL: {} of {} pack(s) mounted", mounted.len(), files.len());
        for m in &mounted {
            println!("  mounted {m}");
        }
        for u in &unmountable {
            println!("  NOT mounted {u}");
        }
        assert!(!mounted.is_empty(), "nothing mounted from {dir}: {unmountable:?}");
        let all = cases();
        let nopack: Vec<&RouteCase> = all.iter().filter(|c| c.accept.is_empty()).collect();
        assert!(nopack.len() >= 10, "the control needs the no-pack corpus: {}", nopack.len());
        let mut offences = Vec::new();
        for c in &nopack {
            let hits = mem.recall_from_packs(c.query, 8).await.unwrap();
            println!("  {:<18} {} row(s)", c.id, hits.len());
            for h in &hits {
                offences.push(format!("{} <- {} @ {:.3}: {}", h.pack_id, c.id, h.similarity, h.text.chars().take(90).collect::<String>()));
            }
        }
        assert!(offences.is_empty(), "rows surfaced for no-pack queries — KILL for those packs' floors:\n{}", offences.join("\n"));
    }
}
