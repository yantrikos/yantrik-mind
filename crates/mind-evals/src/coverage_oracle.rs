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

fn c(
    id: &'static str,
    query: &'static str,
    accept: &'static [&'static str],
    note: &'static str,
) -> RouteCase {
    RouteCase {
        id,
        query,
        accept,
        note,
    }
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
            &[&format!(
                "{} — one placeholder row; the coverage list is the fixture.",
                p.name
            )],
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
        if self.pack_total == 0 {
            0.0
        } else {
            self.pack_agree as f64 / self.pack_total as f64
        }
    }
    pub fn abstention(&self) -> f64 {
        if self.nopack_total == 0 {
            0.0
        } else {
            self.nopack_abstain as f64 / self.nopack_total as f64
        }
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
            if self.agreement() >= 0.80 {
                "GREEN"
            } else {
                "RED"
            },
            self.nopack_abstain,
            self.nopack_total,
            self.abstention(),
            if self.abstention() >= 0.90 {
                "GREEN"
            } else {
                "RED"
            },
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
                if case.accept.is_empty() {
                    score.nopack_total += 1
                } else {
                    score.pack_total += 1
                }
                continue;
            }
        };
        let top = ranked
            .first()
            .map(|m| {
                format!(
                    "{}@{:.2} ← “{}”",
                    m.pack_id,
                    m.sim,
                    m.phrase.chars().take(44).collect::<String>()
                )
            })
            .unwrap_or_else(|| "—".into());
        let second = ranked
            .get(1)
            .map(|m| format!("{}@{:.2}", m.pack_id, m.sim))
            .unwrap_or_else(|| "—".into());
        let leased = route.leased().map(str::to_string);
        let ok = if case.accept.is_empty() {
            score.nopack_total += 1;
            let ok = leased.is_none();
            if ok {
                score.nopack_abstain += 1
            }
            ok
        } else {
            score.pack_total += 1;
            let ok = leased.as_deref().is_some_and(|l| case.accept.contains(&l));
            if ok {
                score.pack_agree += 1
            }
            ok
        };
        score.lines.push(format!(
            "  {} {:<16} {:<20} top {top} · 2nd {second}{}",
            if ok { "OK  " } else { "MISS" },
            case.id,
            route.label(),
            if case.note.is_empty() {
                String::new()
            } else {
                format!(" · {}", case.note)
            }
        ));
    }
    score
}

// ── THE LIVE SPLIT (E.PK3c) ──────────────────────────────────────────────────────────────────
//
// The frozen corpus above says so in its own header: its labels were written from the packs'
// coverage lists, so they share lineage with the coverage authors, and 35/38 may be measuring
// SELF-CONSISTENCY rather than correctness. This split is the antidote — queries the mind was
// actually asked, taken verbatim from the box's flight recorder (`pack_route_shadow.goal`), with
// the ranking the box itself produced recorded beside each one.
//
// Two rules keep it honest. The labels are NOT written here by the same hand that reads the
// routes: they come from an independent witness who was sent the queries without the rankings
// (Doctrine 3), and until they arrive `label` is None and nothing is scored. And nothing is ever
// TUNED on this split: the policy, the floor, the margin and the frozen corpus are untouched by
// anything in this module. A policy revision earns its own pre-registration and its own bar.

/// One query the mind was really asked, and what the box's router really did with it.
#[derive(Debug, Clone)]
pub struct LiveCase {
    pub id: &'static str,
    /// Verbatim from the recorder. Never edited to route better.
    pub query: &'static str,
    /// The independent witness's answer: `Some("pack-id")`, `Some("NONE")`, or None until labelled.
    pub label: Option<&'static str>,
    /// What the BOX did: (verdict, top three as "pack@sim"). Recorded so that a divergence between
    /// this harness's fixture library and the real packs is visible rather than assumed away.
    pub box_verdict: &'static str,
    pub box_top: &'static [&'static str],
}

/// Every live routing decision the box has recorded since the router shipped. Not sampled, not
/// filtered: all of them. n is tiny and stays stated — the value is the harness and the first
/// honest datapoint of a split that grows from the recorder by itself.
pub fn live_cases() -> Vec<LiveCase> {
    vec![
        LiveCase {
            id: "live-ms-buffering",
            query: "what coyote time and input buffering should a 2D platformer use, in milliseconds?",
            label: Some("yantrik/game-feel-craft@0.1.0"),
            box_verdict: "abstain:tie",
            box_top: &["yantrik/c-safety@0.1.0@0.60", "yantrik/python-stdlib@0.1.0@0.58", "yantrik/game-feel-craft@0.1.0@0.55"],
        },
        LiveCase {
            id: "live-npm-skills",
            query: "which of my saved skills could fetch the npm download counts for saga-mcp this week?",
            label: Some("NONE"),
            box_verdict: "lease",
            box_top: &["yantrik/mcp-spec@0.3.2@0.56", "yantrik/c-safety@0.1.0@0.48", "yantrik/python-stdlib@0.1.0@0.40"],
        },
        LiveCase {
            id: "live-coyote-roughly",
            query: "what coyote time should my 2D platformer use, roughly?",
            label: Some("yantrik/game-feel-craft@0.1.0"),
            box_verdict: "lease",
            box_top: &["yantrik/game-feel-craft@0.1.0@0.56", "yantrik/python-stdlib@0.1.0@0.40", "yantrik/java-modern@0.1.0@0.28"],
        },
        LiveCase {
            id: "live-coyote-jump",
            query: "what coyote time and jump buffering should my 2D platformer use",
            label: Some("yantrik/game-feel-craft@0.1.0"),
            box_verdict: "lease",
            box_top: &["yantrik/game-feel-craft@0.1.0@0.75", "yantrik/c-safety@0.1.0@0.53", "yantrik/python-stdlib@0.1.0@0.52"],
        },
    ]
}

// ── GROWING THE SPLIT FROM THE RECORDER (E.PK3d) ────────────────────────────────────────────────
//
// E.PK3c said the live split "accumulates from the recorder by itself" while its four cases had in
// fact been read off a dump and typed in by hand. That is fine for four and is not a mechanism.
// This is the mechanism.
//
// The type below deliberately has NOWHERE TO PUT A LABEL. That is not an oversight: a label must
// come from a witness who has not seen the routes, so a label that could be computed in this repo
// — from the ranking sitting right there in the same struct — would not be a witness's answer at
// all. Making it structurally impossible is stronger than promising not to.

/// What the recorder writes in place of a goal it will not hold. Not a query, and never a case.
const REDACTED_GOAL: &str = "[redacted-secret]";

/// Would sending this query to an outside witness be a mistake?
///
/// Delegates to the ONE shared detector (`mind_types::first_sensitive`), which is the whole point:
/// memory-write refusal, observability redaction, egress denial and this eval gate all read the
/// same finding, so an improvement lands at four boundaries at once (E.SEC1).
///
/// The first version of this gate withheld ANY query containing a run of twelve or more digits.
/// That caught the card number and also every order id, tracking number and epoch timestamp — the
/// blanket rejection Codex named as "the current failure mode wearing a new coat". The shared
/// detector distinguishes them: a payment card needs Luhn AND a card industry digit, and a bare
/// number needs card/PIN/CVV wording nearby to count as one.
pub fn looks_sensitive(query: &str) -> bool {
    mind_types::first_sensitive(query).is_some()
}

/// What a decision log yields: the cases, and how many queries were WITHHELD as possibly sensitive.
/// The count is carried rather than dropped, because a corpus that silently shrinks is a corpus
/// nobody can reason about.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LiveCorpus {
    pub routes: Vec<ExtractedRoute>,
    pub withheld: usize,
}

/// One live routing decision, lifted out of a decision log exactly as the recorder holds it, and
/// unlabelled by construction.
///
/// "Exactly as the recorder holds it" is narrower than "as the person typed it", and the gap is
/// upstream of this file. `DecisionEvent::sanitized` applies `brief(goal, 160)` on append, which
/// does two things a live corpus has to know about:
///
/// * it TRIMS surrounding whitespace and TRUNCATES past 160 characters (adding an ellipsis), so a
///   long question reaches this corpus shortened and would be labelled and scored on less than the
///   router actually routed;
/// * it REDACTS: a goal containing secret-shaped text is replaced wholesale by `[redacted-secret]`.
///
/// The first is a fidelity limit the split inherits and E.PK3d records. The second is not a
/// query at all, so it is dropped here rather than carried: a corpus row reading
/// `[redacted-secret]` could never be labelled, and must never be sent to a witness.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedRoute {
    /// The query the mind was actually asked, exactly as the recorder holds it. Never edited,
    /// normalised or tidied — a query that routes badly because of its punctuation is evidence.
    pub query: String,
    /// The verdict the router reached at the time (`lease`, `abstain:tie`, …).
    pub verdict: String,
    /// The ranking it recorded, as `pack@sim` strings in the recorder's own format.
    pub top: Vec<String>,
    /// How many times this exact query text was routed. Repeats are demand, not duplicates.
    pub occurrences: usize,
    /// True when two routings of the SAME text produced different top packs — which would mean the
    /// catalog or the embedder moved underneath, and is worth knowing before anything is labelled.
    pub rankings_differ: bool,
}

/// Every distinct live routing decision in a decision log, oldest first by first appearance.
///
/// Reads through the VERIFIED chain: a log that does not verify yields an error rather than a
/// best-effort scrape, because a corpus grown from unverified lines is a corpus of unknown
/// provenance. Non-routing events are ignored; a routing event with no goal text cannot become a
/// case and is skipped.
pub fn extract_live_routes(log: &std::path::Path) -> Result<LiveCorpus, String> {
    let events = mind_observability::read_events_verified(log).map_err(|bad| {
        format!(
            "{} does not verify at line {bad} — refusing to grow a corpus from an unverified log",
            log.display()
        )
    })?;
    let mut out = LiveCorpus::default();
    for e in events.into_iter().filter(|e| e.kind == "pack_route_shadow") {
        // A redacted goal is not a question anyone asked — it is the recorder's refusal to hold
        // one — so it can never become a case (E.PK3d).
        let Some(query) = e
            .goal
            .filter(|g| !g.trim().is_empty() && g.trim() != REDACTED_GOAL)
        else {
            continue;
        };
        // ...and one the recorder DID hold may still be nobody's business but the household's.
        if looks_sensitive(&query) {
            out.withheld += 1;
            continue;
        }
        let verdict = e.verdict.unwrap_or_default();
        let top = e.candidates;
        match out.routes.iter_mut().find(|r| r.query == query) {
            Some(seen) => {
                seen.occurrences += 1;
                if seen.top.first() != top.first() {
                    seen.rankings_differ = true;
                }
            }
            None => out.routes.push(ExtractedRoute {
                query,
                verdict,
                top,
                occurrences: 1,
                rankings_differ: false,
            }),
        }
    }
    Ok(out)
}

/// What goes to the WITNESS: the queries, numbered, and nothing else.
///
/// No verdict, no ranking, no similarity, no pack name — everything that could tell someone what
/// the router already did is withheld, because a label informed by the answer is not a label. This
/// is the exact text to send; it is rendered by the same code that reads the log so the two cannot
/// drift apart.
/// Scan a FULLY RENDERED export artifact, and fail closed.
///
/// The per-query gate (`looks_sensitive`) runs before a query enters the corpus. This runs after
/// everything is assembled, because rendering concatenates: fields that were each clean can sit
/// together in the output and form something that is not, and only the finished artifact shows
/// what actually leaves. Reports KINDS and a count, never a value, never an offset into content
/// nobody may see (E.SEC1b, Codex point 6).
pub fn scan_export_artifact(artifact: &str) -> Result<(), String> {
    let found = mind_types::sensitive_findings(artifact);
    if found.is_empty() {
        return Ok(());
    }
    let mut kinds: Vec<&str> = found.iter().map(|f| f.kind.label()).collect();
    kinds.sort_unstable();
    kinds.dedup();
    Err(format!(
        "export refused: {} finding(s) [{}]",
        found.len(),
        kinds.join(", ")
    ))
}

/// The witness prompt, or a refusal. Never a rendered artifact that has not been scanned.
///
/// Fail-closed: a caller cannot obtain the string without the scan having passed, because there is
/// no other function that returns it.
pub fn render_for_witness_checked(corpus: &LiveCorpus) -> Result<String, String> {
    let rendered = render_for_witness(corpus);
    scan_export_artifact(&rendered)?;
    Ok(rendered)
}

/// The case table, or a refusal. Same rule as [`render_for_witness_checked`].
pub fn render_cases_checked(corpus: &LiveCorpus) -> Result<String, String> {
    let rendered = render_cases(corpus);
    scan_export_artifact(&rendered)?;
    Ok(rendered)
}

pub fn render_for_witness(corpus: &LiveCorpus) -> String {
    let mut out = String::from(
        "Label each query with ONE answer: a single pack id, or NONE if no pack would materially \
         help answer it. You are deliberately not being shown what the router did.\n\n",
    );
    for (i, r) in corpus.routes.iter().enumerate() {
        out.push_str(&format!("({}) {}\n", i + 1, r.query));
    }
    if corpus.withheld > 0 {
        out.push_str(&format!(
            "\n({} further quer{} withheld by this host as possibly sensitive and not sent.)\n",
            corpus.withheld,
            if corpus.withheld == 1 {
                "y was"
            } else {
                "ies were"
            }
        ));
    }
    out
}

/// What goes into THIS FILE: `LiveCase` rows with `label: None`, ready for a witness's answers to
/// be typed in beside them. The ranking is carried here — where it is evidence — and never in the
/// text above, where it would be contamination.
pub fn render_cases(corpus: &LiveCorpus) -> String {
    let mut out = String::new();
    for (i, r) in corpus.routes.iter().enumerate() {
        out.push_str(&format!(
            "        LiveCase {{\n            id: \"live-{}\",\n            query: {:?},\n            label: None,\n            box_verdict: {:?},\n            box_top: &[{}],\n        }},\n",
            i + 1,
            r.query,
            r.verdict,
            r.top.iter().map(|t| format!("{t:?}")).collect::<Vec<_>>().join(", ")
        ));
    }
    out
}

/// The live split's result. Reported with n said out loud, because four is not a measurement of
/// anything and a rate over four would read as though it were.
#[derive(Debug, Default)]
pub struct LiveScore {
    pub labelled: usize,
    pub agree: usize,
    pub unlabelled: usize,
    /// Queries where this harness's fixture library ranked a different pack first than the box did.
    pub fixture_box_divergence: Vec<String>,
    pub lines: Vec<String>,
}

impl LiveScore {
    pub fn render(&self) -> String {
        let mut out = String::new();
        for l in &self.lines {
            out.push_str(l);
            out.push('\n');
        }
        out.push_str(&format!(
            "LIVE SPLIT (E.PK3c): {} of {} labelled queries agree; {} still unlabelled. n is 4 — this is a datapoint, not a rate.\n",
            self.agree, self.labelled, self.unlabelled
        ));
        if self.fixture_box_divergence.is_empty() {
            out.push_str("  fixture library reproduces the box's top pack on every live query\n");
        } else {
            out.push_str("  FIXTURE/BOX DIVERGENCE (this harness is not a faithful proxy for those queries):\n");
            for d in &self.fixture_box_divergence {
                out.push_str(&format!("    {d}\n"));
            }
        }
        out
    }
}

/// Route every live query through the CURRENT policy and compare with (a) the independent labels,
/// where they exist, and (b) what the box actually did. Changes nothing and tunes nothing.
pub async fn run_live_split(mem: &dyn MemoryFacade) -> LiveScore {
    let mut score = LiveScore::default();
    for case in live_cases() {
        let (ranked, route) = match mem.route_packs(case.query).await {
            Ok(x) => x,
            Err(e) => {
                score.lines.push(format!("  {} ERROR {e}", case.id));
                continue;
            }
        };
        let here_top = ranked.first().map(|m| m.pack_id.clone());
        let box_top_pack = case
            .box_top
            .first()
            .and_then(|s| s.rsplit_once('@'))
            .map(|(p, _)| p.to_string());
        if let (Some(h), Some(b)) = (&here_top, &box_top_pack) {
            if h != b {
                score.fixture_box_divergence.push(format!(
                    "{}: fixture ranked {h} first, the box ranked {b}",
                    case.id
                ));
            }
        }
        let leased = route.leased().map(str::to_string);
        let verdict = match &case.label {
            None => {
                score.unlabelled += 1;
                "UNLABELLED".to_string()
            }
            Some(label) => {
                score.labelled += 1;
                let ok = match *label {
                    "NONE" => leased.is_none(),
                    want => leased.as_deref() == Some(want),
                };
                if ok {
                    score.agree += 1;
                }
                if ok {
                    "AGREE".to_string()
                } else {
                    format!("DISAGREE (witness said {label})")
                }
            }
        };
        let top3 = ranked
            .iter()
            .take(3)
            .map(|m| format!("{}@{:.2}", m.pack_id, m.sim))
            .collect::<Vec<_>>()
            .join(" · ");
        score.lines.push(format!(
            "  {:<20} {:<28} here: {:<14} [{top3}]\n      box: {:<14} [{}]",
            case.id,
            verdict,
            route.label(),
            case.box_verdict,
            case.box_top.join(" · ")
        ));
    }
    score
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unique per RUN and removed when the test ends (E.SCRATCH1). The `remove_dir_all` that used
    /// to stand here existed because the pid-keyed path was REUSED across runs; a path that is
    /// unique has nothing to clear, and removing it now would delete the directory just created.
    fn scratch(tag: &str) -> mind_types::scratch::Scratch {
        mind_types::scratch::dir(&format!("p3_{tag}"))
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
                assert!(
                    lib.iter().any(|l| l == a),
                    "{}: accepts unknown pack {a}",
                    c.id
                );
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
        assert_eq!(
            empty,
            PackRoute::Abstain {
                reason: AbstainReason::NoPacks,
                best: None
            }
        );
        mem.set_pack_library(dir.to_str().unwrap()).await.unwrap();
        let cat = mem.available_packs().await.unwrap();
        assert_eq!(cat.len(), 12, "{cat:?}");
        assert!(cat.iter().all(|e| !e.mounted));
        let (ranked, route) = mem
            .route_packs("coyote time and jump buffering for a platformer")
            .await
            .unwrap();
        assert_eq!(ranked.len(), 12);
        assert!(["lease", "abstain:below_floor", "abstain:tie"].contains(&route.label()));
        assert!(
            mem.mounted_packs().await.unwrap().is_empty(),
            "routing mounted something"
        );
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

    /// E.PK3c: the live split runs against the CURRENT policy and reports. Ungated, because it
    /// asserts nothing about agreement until an independent witness has labelled the queries — what
    /// it pins today is that every live query still routes deterministically, and whether this
    /// harness's fixture library is a faithful proxy for the real packs on those queries.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_live_split_runs_and_reports_against_the_unchanged_policy() {
        let dir = scratch("live");
        seal_library(&dir).unwrap();
        let mem = mind_memory::MemoryHandle::spawn(":memory:", 64).unwrap();
        mem.set_pack_library(dir.to_str().unwrap()).await.unwrap();
        let score = run_live_split(&mem).await;
        println!("{}", score.render());
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            score.labelled + score.unlabelled,
            live_cases().len(),
            "every live query is accounted for"
        );
        // Until the labels arrive nothing is scored — and that is stated, not silently skipped.
        if score.labelled == 0 {
            assert_eq!(
                score.unlabelled, 4,
                "the live set is the four the recorder holds"
            );
        } else {
            assert!(score.labelled >= 1);
        }
    }

    /// E.SEC1, BOUNDARY 4 of 4: eval withholding reads the same finding as memory, observability
    /// and egress. Asserted here because `mind-evals` depends on `mind-conversation` and the other
    /// three boundaries are tested there — the cycle is why this half lives on its own.
    #[test]
    fn the_eval_gate_uses_the_shared_finding_and_not_a_blanket_number_rule() {
        // Caught — and by the shared detector, not by counting digits.
        for text in [
            "my password is hunter2swordfish",
            "my card pin is 4471-9302-1122-8890",
            "charge 4111 1111 1111 1111 today",
        ] {
            assert!(looks_sensitive(text), "must be withheld: {text:?}");
            assert!(
                mind_types::first_sensitive(text).is_some(),
                "and by the SHARED detector: {text:?}"
            );
        }
        // NOT caught: the blanket twelve-digit rule used to withhold every one of these.
        for text in [
            "order 9876543210987 shipped",
            "the timestamp was 1756170000000",
            "tracking 1Z999AA10123456784",
            "uuid 550e8400-e29b-41d4-a716-446655440000",
            "the box is at 192.168.4.90",
            "use 80-100 ms of coyote time",
            "remind me about the task-list and asian food recipes",
        ] {
            assert!(
                !looks_sensitive(text),
                "a corpus must be able to hold this: {text:?}"
            );
        }
    }

    /// E.PK3d: the extractor copies the recorder VERBATIM, counts repeats instead of dropping them,
    /// notices when one query's ranking moves between routings, and refuses a log that does not
    /// verify. A corpus grown from a scrape of unverified lines has unknown provenance.
    #[test]
    fn the_extractor_lifts_live_queries_without_touching_them() {
        use mind_observability::{DecisionEvent, DecisionLog};
        let dir = mind_types::scratch::dir("pk3d");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("d.jsonl");
        let log = DecisionLog::open(&path);
        let route = |goal: &str, verdict: &str, top: &[&str]| {
            let mut e = DecisionEvent::new("t", "pack_route_shadow");
            e.goal = Some(goal.to_string());
            e.verdict = Some(verdict.to_string());
            e.candidates = top.iter().map(|s| s.to_string()).collect();
            e
        };
        // Two routings of one query, one of another, and noise that must be ignored.
        log.record(route(
            "  what coyote time, roughly?  ",
            "lease",
            &["yantrik/game-feel-craft@0.1.0@0.56"],
        ));
        log.record(DecisionEvent::new("t", "tool_predicted"));
        log.record(route(
            "which saved skill fetches npm counts for saga-mcp?",
            "lease",
            &["yantrik/mcp-spec@0.3.2@0.56"],
        ));
        log.record(route(
            "  what coyote time, roughly?  ",
            "lease",
            &["yantrik/game-feel-craft@0.1.0@0.56"],
        ));
        let mut e = DecisionEvent::new("t", "pack_route_shadow");
        e.verdict = Some("abstain:no_packs".into()); // no goal: cannot become a case
        log.record(e);

        let routes = extract_live_routes(&path).unwrap().routes;
        assert_eq!(
            routes.len(),
            2,
            "one row per distinct query, noise ignored: {routes:?}"
        );
        // EXACTLY WHAT THE RECORDER HOLDS. The surrounding spaces are gone — and NOT by this
        // extractor's doing: `DecisionEvent::sanitized` trims every free-text field on append, so
        // they were already gone before the log was read. The distinction matters enough to assert
        // rather than describe (E.PK3d).
        assert_eq!(routes[0].query, "what coyote time, roughly?");
        assert_eq!(
            routes[0].occurrences, 2,
            "a repeat is demand, not a duplicate"
        );
        assert!(!routes[0].rankings_differ);
        assert_eq!(
            routes[1].query,
            "which saved skill fetches npm counts for saga-mcp?"
        );
        assert_eq!(routes[1].occurrences, 1);
        // Order is first-appearance, so the output is stable across runs.
        assert_eq!(
            routes.iter().map(|r| r.occurrences).collect::<Vec<_>>(),
            vec![2, 1]
        );

        // A ranking that MOVES under one query is flagged rather than silently overwritten.
        log.record(route(
            "  what coyote time, roughly?  ",
            "lease",
            &["yantrik/c-safety@0.1.0@0.61"],
        ));
        let routes = extract_live_routes(&path).unwrap().routes;
        assert!(
            routes[0].rankings_differ,
            "the catalog or the embedder moved: that must be visible"
        );

        // THE FIDELITY LIMIT, pinned rather than described: the recorder caps a goal at 160
        // characters, so a long question reaches the corpus SHORTENED while the router routed all
        // of it live. A case built from it would be labelled on less than the mind was asked.
        let long = format!(
            "what coyote time should my platformer use {}",
            "and also ".repeat(40)
        );
        assert!(long.chars().count() > 160);
        log.record(route(
            &long,
            "lease",
            &["yantrik/game-feel-craft@0.1.0@0.56"],
        ));
        let routes = extract_live_routes(&path).unwrap().routes;
        let stored = routes
            .iter()
            .find(|r| {
                r.query
                    .starts_with("what coyote time should my platformer use")
            })
            .expect("the long one is there");
        assert!(
            stored.query.ends_with('…'),
            "the recorder marks what it cut: {}",
            stored.query
        );
        assert!(
            stored.query.chars().count() < long.chars().count(),
            "so the corpus holds LESS than the router saw"
        );
        assert_eq!(
            stored.query.chars().count(),
            161,
            "160 characters plus the ellipsis brief() adds"
        );

        // THE ONE THAT SURPRISED ME. The recorder does NOT redact this: `contains_secret` is a
        // marker detector and a bare run of digits carries no marker, so the card number is written
        // to the log verbatim — and this protocol's whole purpose is to send queries to an OUTSIDE
        // witness. The split therefore gates on its own, before anything leaves the building.
        log.record(route(
            "my card pin is 4471-9302-1122-8890 what coyote time",
            "lease",
            &["yantrik/game-feel-craft@0.1.0@0.56"],
        ));
        let corpus = extract_live_routes(&path).unwrap();
        // E.SEC1 moved the defence EARLIER than this gate. The recorder now recognises the card
        // context and writes `[redacted-secret]` in place of the goal, so the number never enters
        // the log at all — and the extractor drops a redacted goal, because it is not a question
        // anyone asked. `withheld` is therefore 0: there was nothing left for the eval gate to
        // withhold. That is the STRONGER outcome, and worth asserting as such rather than
        // restoring the weaker one that assumed the secret got as far as this corpus.
        // THE PROBE MUST BE UNFORGEABLE BY THE LEDGER'S OWN BOOKKEEPING. `4471` is four decimal
        // digits, and every line carries a 64-char chain hash (hex, so digits qualify) and a
        // nanosecond timestamp. Measured over real DecisionLog files, a six-record log contains
        // ~59 distinct 4-digit strings, so a bare `4471` matches by accident about once in 170
        // runs — which is the entire "redaction flake" that went unexplained for a day. The
        // recorder was never at fault: `contains_secret` is pure and has no env gate, so it cannot
        // redact intermittently. A separated or full-length card cannot occur in hex or in a
        // timestamp, so these probes keep the "anywhere in the file" strength without the dice.
        let raw = std::fs::read_to_string(&path).unwrap();
        for probe in ["4471-9302", "4471930211228890", "9302-1122-8890"] {
            assert!(!raw.contains(probe), "the RECORDER must not hold it: {probe}");
        }
        // And the redaction must have HAPPENED, not merely left no trace: an absence test alone
        // passes just as well when the recorder wrote nothing at all.
        assert!(
            raw.contains(REDACTED_GOAL),
            "the goal must be present AS a redaction, not simply missing"
        );
        assert!(
            !corpus
                .routes
                .iter()
                .any(|r| r.query.contains("4471") || r.query.contains("redacted")),
            "neither a secret nor a redaction marker may become a case: {:?}",
            corpus.routes
        );
        let sent = render_for_witness(&corpus);
        assert!(!sent.contains("4471"), "and nothing reaches the request");
        // The eval gate still stands as the LAST line of defence, for anything the recorder was
        // willing to hold — proved directly, since the recorder now stops this one earlier.
        assert!(looks_sensitive(
            "my card pin is 4471-9302-1122-8890 what coyote time"
        ));

        // An unverified log yields an error, never a best-effort scrape.
        {
            use std::io::Write;
            std::fs::OpenOptions::new().append(true).open(&path).unwrap()
                .write_all(b"{\"chain\":\"deadbeef\",\"event\":{\"trace_id\":\"t\",\"ts_ms\":1,\"kind\":\"pack_route_shadow\",\"goal\":\"forged\"}}\n").unwrap();
        }
        let err = extract_live_routes(&path).unwrap_err();
        assert!(err.contains("does not verify"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// E.PK3d, THE PROTOCOL'S ONE STRUCTURAL GUARANTEE: what goes to the witness carries the
    /// queries and NOTHING that could reveal what the router already did. A label informed by the
    /// answer is not a label, and this is the check that keeps the request honest as the renderer
    /// changes.
    #[test]
    fn the_witness_is_shown_the_queries_and_nothing_else() {
        let routes = vec![
            ExtractedRoute {
                query: "what coyote time and input buffering, in milliseconds?".into(),
                verdict: "abstain:tie".into(),
                top: vec![
                    "yantrik/c-safety@0.1.0@0.60".into(),
                    "yantrik/game-feel-craft@0.1.0@0.55".into(),
                ],
                occurrences: 3,
                rankings_differ: false,
            },
            ExtractedRoute {
                query: "which saved skill fetches npm counts for saga-mcp?".into(),
                verdict: "lease".into(),
                top: vec!["yantrik/mcp-spec@0.3.2@0.56".into()],
                occurrences: 1,
                rankings_differ: false,
            },
        ];
        let corpus = LiveCorpus {
            routes: routes.clone(),
            withheld: 0,
        };
        let sent = render_for_witness(&corpus);
        for q in routes.iter().map(|r| &r.query) {
            assert!(
                sent.contains(q.as_str()),
                "every query must be asked: {sent}"
            );
        }
        // Nothing about the routes may leak: no verdict, no pack, no similarity, no repeat count.
        for leak in [
            "abstain",
            "lease",
            "c-safety",
            "game-feel",
            "mcp-spec",
            "0.60",
            "0.55",
            "0.56",
            "yantrik/",
        ] {
            assert!(
                !sent.contains(leak),
                "the witness must not be told {leak:?}:\n{sent}"
            );
        }
        assert!(
            !sent.contains("occurrences") && !sent.contains("3"),
            "not even how often it was asked:\n{sent}"
        );
        assert!(
            sent.contains("NONE"),
            "the NONE option must be offered explicitly, not inferred"
        );

        // The rows that go into THIS file do carry the ranking — where it is evidence — and never
        // a label, because there is nowhere for one to come from yet.
        let rows = render_cases(&corpus);
        assert!(rows.contains("label: None"), "{rows}");
        assert!(rows.matches("label: None").count() == 2);
        assert!(
            rows.contains("yantrik/c-safety@0.1.0@0.60"),
            "the ranking is kept as evidence: {rows}"
        );
        assert!(rows.contains("abstain:tie"));
    }

    /// E.PK3c: the split's own integrity. Every query is verbatim from the recorder and every case
    /// carries what the box did, so a label can never be quietly written to fit a route.
    #[test]
    fn the_live_split_is_shaped_so_labels_cannot_be_fitted_to_routes() {
        let live = live_cases();
        assert_eq!(live.len(), 4, "all four recorded routes, not a sample");
        for c in &live {
            assert!(!c.query.trim().is_empty());
            assert!(
                c.box_top.len() >= 3,
                "{}: the box's ranking is recorded beside the query",
                c.id
            );
            assert!(
                [
                    "lease",
                    "abstain:tie",
                    "abstain:below_floor",
                    "abstain:no_packs",
                    "abstain:router_error"
                ]
                .contains(&c.box_verdict),
                "{}: {} is not a verdict the router can produce",
                c.id,
                c.box_verdict
            );
            if let Some(l) = c.label {
                assert!(
                    l == "NONE" || l.starts_with("yantrik/"),
                    "{}: a label is a pack id or NONE, got {l}",
                    c.id
                );
            }
        }
        // The live queries must not have been copied from the frozen corpus: a split that repeats
        // the corpus measures the corpus again.
        let corpus: Vec<&str> = cases().iter().map(|c| c.query).collect();
        for c in &live {
            assert!(
                !corpus.contains(&c.query),
                "{} is already in the frozen corpus",
                c.id
            );
        }
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
            .filter(|p| p.extension().is_some_and(|x| x == "ydbpack"))
            .collect();
        files.sort();
        let (mut mounted, mut unmountable) = (Vec::new(), Vec::new());
        for f in &files {
            match mem.mount_pack(f.to_str().unwrap()).await {
                Ok(id) => mounted.push(id),
                Err(e) => {
                    unmountable.push(format!("{}: {e}", f.file_name().unwrap().to_string_lossy()))
                }
            }
        }
        println!(
            "ATTACH-HARM CONTROL: {} of {} pack(s) mounted",
            mounted.len(),
            files.len()
        );
        for m in &mounted {
            println!("  mounted {m}");
        }
        for u in &unmountable {
            println!("  NOT mounted {u}");
        }
        assert!(
            !mounted.is_empty(),
            "nothing mounted from {dir}: {unmountable:?}"
        );
        let all = cases();
        let nopack: Vec<&RouteCase> = all.iter().filter(|c| c.accept.is_empty()).collect();
        assert!(
            nopack.len() >= 10,
            "the control needs the no-pack corpus: {}",
            nopack.len()
        );
        let mut offences = Vec::new();
        for c in &nopack {
            let hits = mem.recall_from_packs(c.query, 8).await.unwrap();
            println!("  {:<18} {} row(s)", c.id, hits.len());
            for h in &hits {
                offences.push(format!(
                    "{} <- {} @ {:.3}: {}",
                    h.pack_id,
                    c.id,
                    h.similarity,
                    h.text.chars().take(90).collect::<String>()
                ));
            }
        }
        assert!(
            offences.is_empty(),
            "rows surfaced for no-pack queries — KILL for those packs' floors:\n{}",
            offences.join("\n")
        );
    }
}

/// E.SEC1b boundary proof 4 of 4 — the eval export gate and the rendered artifact (Codex points 4, 6).
#[cfg(test)]
mod sec1b_boundary {
    use super::*;

    #[test]
    fn the_export_gate_withholds_a_secret_query_and_still_admits_ordinary_ones() {
        for secret in [
            "my password is hunter2",
            "ghp_SECRET12345",
            "charge my card 4471 9302 1122 8890",
        ] {
            assert!(looks_sensitive(secret), "must be withheld: {secret:?}");
        }
        // The control. The FIRST version of this gate withheld any run of twelve or more digits,
        // which caught every order id, tracking number and epoch — Codex named it "the current
        // failure mode wearing a new coat".
        for ordinary in [
            "where is order 100000000000",
            "track 1Z999AA10123456784",
            "what happened at 1756170000000",
            "asian food recipes",
        ] {
            assert!(
                !looks_sensitive(ordinary),
                "must still be exportable: {ordinary:?}"
            );
        }
    }

    #[test]
    fn a_rendered_artifact_is_scanned_whole_and_refuses_rather_than_ships() {
        let corpus = LiveCorpus {
            routes: vec![ExtractedRoute {
                query: "my password is hunter2".into(),
                verdict: "NONE".into(),
                top: vec![],
                occurrences: 1,
                rankings_differ: false,
            }],
            withheld: 0,
        };
        // The per-query gate is not the last word: this corpus was built by hand, as a caller with
        // a bug could build one. The artifact scan is what stands between it and the wire.
        let err =
            render_for_witness_checked(&corpus).expect_err("a secret in the artifact must refuse");
        assert!(
            err.contains("credential-phrase"),
            "the refusal names kinds: {err}"
        );
        assert!(!err.contains("hunter2"), "and never the value: {err}");
        assert!(
            render_cases_checked(&corpus).is_err(),
            "the case table is the same artifact by another name"
        );

        // The control: a clean corpus renders, so the gate is not simply closed.
        let clean = LiveCorpus {
            routes: vec![ExtractedRoute {
                query: "where is order 100000000000".into(),
                verdict: "NONE".into(),
                top: vec![],
                occurrences: 1,
                rankings_differ: false,
            }],
            withheld: 2,
        };
        let out = render_for_witness_checked(&clean).expect("a clean corpus must render");
        assert!(out.contains("order 100000000000"));
        assert!(
            out.contains("2 further quer"),
            "and still says what it withheld"
        );
    }
}
