//! paper — the mind reads research papers and articles, once, into memory. Deterministic-primary:
//! fetch + HTML/PDF text extraction + section skeleton cost zero LLM; distillation is one pass.
//! Text is cached under the workdir so later questions can do targeted re-reads from disk.

use std::path::PathBuf;

fn papers_dir() -> PathBuf {
    let d = PathBuf::from(std::env::var("YM_CODE_DIR").unwrap_or_else(|_| "/var/lib/yantrik-mind/repos".into()))
        .join("_papers");
    let _ = std::fs::create_dir_all(&d);
    d
}

/// Stable short key from a paper/article URL: arxiv id when present, else last meaningful path
/// segment, alnum-only lowercase.
pub fn paper_key(url: &str) -> String {
    let u = url.trim_end_matches('/');
    if let Some(p) = u.find("arxiv.org/") {
        let tail = &u[p + 10..];
        let id: String = tail
            .trim_start_matches("abs/").trim_start_matches("pdf/").trim_start_matches("html/")
            .chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
        if id.len() >= 8 {
            return format!("arxiv{}", id.replace('.', ""));
        }
    }
    let seg = u.rsplit('/').find(|s| s.len() > 3).unwrap_or("paper");
    let mut k: String = seg.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase();
    k.truncate(40);
    if k.is_empty() { k = "paper".into(); }
    k
}

fn drop_tag_bodies(html: String, tag: &str) -> String {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    // ascii lowercase preserves byte offsets (to_lowercase() can change lengths and break slicing)
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut pos = 0usize;
    while let Some(i) = lower[pos..].find(&open) {
        let i = pos + i;
        out.push_str(&html[pos..i]);
        match lower[i..].find(&close) {
            Some(j) => pos = i + j + close.len(),
            None => { pos = html.len(); break; }
        }
    }
    out.push_str(&html[pos..]);
    out
}

fn strip_html(html: &str) -> String {
    // drop script/style/nav bodies, then tags, then decode the entities that matter for prose
    let mut rest = html.to_string();
    for tag in ["script", "style", "nav", "header", "footer"] {
        rest = drop_tag_bodies(rest, tag);
    }
    let mut out = String::with_capacity(rest.len() / 2);
    let mut in_tag = false;
    let mut last_nl = false;
    for c in rest.chars() {
        match c {
            '<' => {
                in_tag = true;
                if !last_nl { out.push('\n'); last_nl = true; }
            }
            '>' => in_tag = false,
            _ if !in_tag => {
                if c == '\n' || c == '\r' {
                    if !last_nl { out.push('\n'); last_nl = true; }
                } else {
                    out.push(c);
                    last_nl = false;
                }
            }
            _ => {}
        }
    }
    out.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">")
        .replace("&quot;", "\"").replace("&#39;", "'").replace("&nbsp;", " ")
}

/// Fetch a paper/article and extract readable text. arxiv abs/pdf URLs are rerouted to the ar5iv
/// HTML rendering (full text, no PDF parsing needed); PDFs fall back to `pdftotext` when present.
/// Returns (title, text). Text is cached to `<workdir>/_papers/<key>.txt` for targeted re-reads.
pub fn fetch_paper(url: &str) -> anyhow::Result<(String, String)> {
    let key = paper_key(url);
    let cache = papers_dir().join(format!("{key}.txt"));
    let fetch_url = if url.contains("arxiv.org/") {
        let id: String = url
            .rsplit('/').next().unwrap_or("")
            .trim_end_matches(".pdf")
            .chars().filter(|c| c.is_ascii_digit() || *c == '.').collect();
        if id.len() >= 8 { format!("https://ar5iv.labs.arxiv.org/html/{id}") } else { url.to_string() }
    } else {
        url.to_string()
    };
    let resp = ureq::get(&fetch_url)
        .set("User-Agent", "Mozilla/5.0 (yantrik-mind research reader)")
        .timeout(std::time::Duration::from_secs(45))
        .call()?;
    let ctype = resp.header("content-type").unwrap_or("").to_lowercase();
    let text = if ctype.contains("pdf") || fetch_url.ends_with(".pdf") {
        // PDF: write to tmp, try pdftotext
        let mut buf: Vec<u8> = Vec::new();
        use std::io::Read;
        resp.into_reader().take(30_000_000).read_to_end(&mut buf)?;
        let tmp = papers_dir().join(format!("{key}.pdf"));
        std::fs::write(&tmp, &buf)?;
        let out = std::process::Command::new("pdftotext")
            .arg("-layout").arg(&tmp).arg("-")
            .output();
        match out {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
            _ => anyhow::bail!("PDF fetched but pdftotext unavailable — give me an HTML link (arxiv abs links work best)"),
        }
    } else {
        let html = resp.into_string()?;
        strip_html(&html)
    };
    // squeeze blank runs, cap size
    let mut clean = String::with_capacity(text.len());
    let mut blanks = 0;
    for line in text.lines() {
        let l = line.trim();
        if l.is_empty() {
            blanks += 1;
            if blanks <= 1 { clean.push('\n'); }
        } else {
            blanks = 0;
            clean.push_str(l);
            clean.push('\n');
        }
    }
    let mut clean: String = clean.chars().take(400_000).collect();
    // Brand-new arXiv papers often have no ar5iv rendering yet (stub page). Fall back to the
    // /abs/ page — an abstract-grounded study is thinner but honest.
    if clean.len() < 5000 && fetch_url.contains("ar5iv") && url.contains("arxiv.org/") {
        let abs_url = url.replace("/pdf/", "/abs/");
        if let Ok(resp2) = ureq::get(&abs_url)
            .set("User-Agent", "Mozilla/5.0 (yantrik-mind research reader)")
            .timeout(std::time::Duration::from_secs(30))
            .call()
        {
            if let Ok(html2) = resp2.into_string() {
                let alt = strip_html(&html2);
                if alt.len() > clean.len() {
                    clean = alt.chars().take(400_000).collect();
                }
            }
        }
    }
    if clean.len() < 800 {
        anyhow::bail!("extracted only {} chars — page may be JS-rendered or paywalled", clean.len());
    }
    let title = clean.lines().find(|l| l.len() > 15).unwrap_or(&key).chars().take(140).collect::<String>();
    std::fs::write(&cache, &clean)?;
    Ok((title, clean))
}

/// Deterministic section skeleton: headers matched by numbering/keywords, with char offsets —
/// tells the distiller (and the reader) the paper's shape for free.
pub fn section_skeleton(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    const HEADS: [&str; 14] = [
        "abstract", "introduction", "related work", "background", "method", "approach",
        "architecture", "experiment", "evaluation", "results", "discussion", "limitations",
        "conclusion", "references",
    ];
    for line in text.lines() {
        let l = line.trim();
        if l.len() > 80 || l.len() < 4 { continue; }
        let low = l.to_lowercase();
        let numbered = l.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) && l.len() < 60;
        if HEADS.iter().any(|h| low == *h || (low.contains(h) && (numbered || low.len() < 30))) {
            if out.last().map(|x: &String| x != l).unwrap_or(true) {
                out.push(l.to_string());
            }
            if out.len() >= 20 { break; }
        }
    }
    out
}

/// Targeted re-read of a cached paper: lines containing any query word (len>=4), with context.
pub fn paper_lookup(key: &str, words: &[String], max_hits: usize) -> Vec<String> {
    let cache = papers_dir().join(format!("{key}.txt"));
    let Ok(text) = std::fs::read_to_string(&cache) else { return vec![] };
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let low = line.to_lowercase();
        if words.iter().any(|w| low.contains(&w.to_lowercase())) {
            let start = i.saturating_sub(1);
            let end = (i + 3).min(lines.len());
            out.push(lines[start..end].join("\n"));
            if out.len() >= max_hits { break; }
        }
    }
    out
}

/// Deterministic arXiv discovery: query the export API (Atom XML), parse entries by string ops —
/// no XML dep. Returns (abs_url, title, abstract) newest-first.
pub fn arxiv_search(query: &str, max: usize) -> anyhow::Result<Vec<(String, String, String)>> {
    let q: String = query
        .chars()
        .map(|c| if c.is_alphanumeric() { c.to_string() } else if c == ' ' { "+".into() } else { format!("%{:02X}", c as u32) })
        .collect();
    let url = format!(
        "http://export.arxiv.org/api/query?search_query=all:{q}&sortBy=submittedDate&sortOrder=descending&max_results={max}"
    );
    let body = ureq::get(&url)
        .set("User-Agent", "yantrik-mind research reader")
        .timeout(std::time::Duration::from_secs(30))
        .call()?
        .into_string()?;
    let mut out = Vec::new();
    for entry in body.split("<entry>").skip(1) {
        let grab = |open: &str, close: &str| -> Option<String> {
            let i = entry.find(open)? + open.len();
            let j = entry[i..].find(close)? + i;
            Some(entry[i..j].split_whitespace().collect::<Vec<_>>().join(" "))
        };
        if let (Some(id), Some(title)) = (grab("<id>", "</id>"), grab("<title>", "</title>")) {
            if id.contains("arxiv.org/abs/") {
                let summary = grab("<summary>", "</summary>").unwrap_or_default();
                out.push((id.replace("http://", "https://"), title, summary));
            }
        }
        if out.len() >= max { break; }
    }
    Ok(out)
}
