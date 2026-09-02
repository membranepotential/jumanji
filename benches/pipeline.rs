//! Criterion benches for `core::pipeline::render` across representative
//! document shapes. Fixtures are synthetic (generated below) except `demo`,
//! which is the real showcase document, so the numbers track a document a
//! user might actually open.
//!
//! Manual `main()` (no `criterion_group!`/`criterion_main!`): before handing
//! control to Criterion we time one throwaway render twice, cold then warm.
//! The first call anywhere in the process pays for syntect's `OnceLock`
//! syntax/theme load and the math CSS/font init; every render after that is
//! warm. Criterion's own warm-up would hide that one-time cost, and it is
//! exactly the number that matters for perceived startup latency.

use std::hint::black_box;
use std::path::Path;
use std::time::Instant;

use criterion::Criterion;

use jumanji::core::pipeline::{self, Options};
use jumanji::core::vault::{Vault, VaultIndex};

/// A vault rooted at a directory that does not exist, with an empty index:
/// every `[[…]]` resolves to "unresolved", which is exactly what a
/// pipeline-*shape* bench wants. `Vault::rooted` (what `core::pipeline`'s own
/// tests use) is `#[cfg(test)]`-only and so invisible to a bench binary; this
/// is its non-test equivalent, minus the (moot, since the root doesn't exist)
/// directory scan.
fn bench_vault() -> Vault {
    let root = Path::new("/nonexistent-bench-vault");
    Vault::new(
        VaultIndex::build(root.to_path_buf(), Vec::new()),
        &root.join("bench.md"),
    )
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const LOREM: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod \
tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud \
exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in \
reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur.";

/// Plain paragraphs with the occasional heading, no fences/math/links — grown
/// until it reaches roughly `target_bytes`.
fn prose(target_bytes: usize) -> String {
    let mut s = String::with_capacity(target_bytes + 512);
    let mut i = 0usize;
    while s.len() < target_bytes {
        if i.is_multiple_of(7) {
            s.push_str(&format!("## Section {}\n\n", i / 7 + 1));
        }
        s.push_str(LOREM);
        s.push_str("\n\n");
        i += 1;
    }
    s
}

fn rust_snippet(n: usize) -> String {
    let lines = [
        format!("// widget {n}: a small state machine"),
        "#[derive(Debug, Clone, PartialEq, Eq)]".to_string(),
        format!("enum State{n} {{"),
        "    Idle,".to_string(),
        "    Running(u32),".to_string(),
        "    Done,".to_string(),
        "}".to_string(),
        String::new(),
        format!("impl State{n} {{"),
        "    fn step(self, input: u32) -> Self {".to_string(),
        "        match self {".to_string(),
        format!("            State{n}::Idle if input > 0 => State{n}::Running(input),"),
        format!("            State{n}::Running(n) if n > 10 => State{n}::Done,"),
        format!("            State{n}::Running(n) => State{n}::Running(n + input),"),
        "            other => other,".to_string(),
        "        }".to_string(),
        "    }".to_string(),
        "}".to_string(),
        String::new(),
        "fn main() {".to_string(),
        format!("    let mut s = State{n}::Idle;"),
        "    for i in 0..20 {".to_string(),
        "        s = s.step(i);".to_string(),
        "        println!(\"step {i}: {s:?}\");".to_string(),
        "    }".to_string(),
        "}".to_string(),
    ];
    lines.join("\n") + "\n"
}

fn python_snippet(n: usize) -> String {
    let lines = [
        "import itertools".to_string(),
        String::new(),
        format!("class Worker{n}:"),
        "    \"\"\"Processes a queue of jobs.\"\"\"".to_string(),
        String::new(),
        "    def __init__(self, capacity: int = 10):".to_string(),
        "        self.capacity = capacity".to_string(),
        "        self.jobs = []".to_string(),
        String::new(),
        "    def enqueue(self, job):".to_string(),
        "        if len(self.jobs) >= self.capacity:".to_string(),
        format!("            raise ValueError(f\"queue {n} is full\")"),
        "        self.jobs.append(job)".to_string(),
        String::new(),
        "    def drain(self):".to_string(),
        "        for job, idx in zip(self.jobs, itertools.count()):".to_string(),
        format!("            print(f\"worker {n}: job {{idx}} -> {{job}}\")"),
        "        self.jobs = []".to_string(),
        String::new(),
        "if __name__ == \"__main__\":".to_string(),
        format!("    w = Worker{n}()"),
        "    for i in range(5):".to_string(),
        "        w.enqueue(i * i)".to_string(),
        "    w.drain()".to_string(),
    ];
    lines.join("\n") + "\n"
}

fn js_snippet(n: usize) -> String {
    let lines = [
        format!("// cache {n}: a tiny LRU"),
        format!("class Lru{n} {{"),
        "  constructor(limit = 8) {".to_string(),
        "    this.limit = limit;".to_string(),
        "    this.map = new Map();".to_string(),
        "  }".to_string(),
        String::new(),
        "  get(key) {".to_string(),
        "    if (!this.map.has(key)) return undefined;".to_string(),
        "    const value = this.map.get(key);".to_string(),
        "    this.map.delete(key);".to_string(),
        "    this.map.set(key, value);".to_string(),
        "    return value;".to_string(),
        "  }".to_string(),
        String::new(),
        "  set(key, value) {".to_string(),
        "    if (this.map.has(key)) this.map.delete(key);".to_string(),
        "    else if (this.map.size >= this.limit) {".to_string(),
        "      this.map.delete(this.map.keys().next().value);".to_string(),
        "    }".to_string(),
        "    this.map.set(key, value);".to_string(),
        "  }".to_string(),
        "}".to_string(),
        String::new(),
        format!("const lru = new Lru{n}();"),
        "for (let i = 0; i < 20; i++) { lru.set(`k${i}`, i * i); }".to_string(),
        "console.log(lru.get('k5'));".to_string(),
    ];
    lines.join("\n") + "\n"
}

fn toml_snippet(n: usize) -> String {
    let lines = [
        format!("[package{n}]"),
        format!("name = \"widget-{n}\""),
        format!("version = \"0.{n}.0\""),
        "edition = \"2024\"".to_string(),
        String::new(),
        "[dependencies]".to_string(),
        "serde = { version = \"1\", features = [\"derive\"] }".to_string(),
        "anyhow = \"1\"".to_string(),
        String::new(),
        "[profile.release]".to_string(),
        "lto = true".to_string(),
        "codegen-units = 1".to_string(),
        String::new(),
        "[[bin]]".to_string(),
        format!("name = \"widget-{n}\""),
        "path = \"src/main.rs\"".to_string(),
    ];
    lines.join("\n") + "\n"
}

fn sh_snippet(n: usize) -> String {
    let lines = [
        "#!/usr/bin/env bash".to_string(),
        "set -euo pipefail".to_string(),
        String::new(),
        format!("# deploy job {n}"),
        format!("JOB_ID={n}"),
        "OUT_DIR=\"/tmp/build-${JOB_ID}\"".to_string(),
        String::new(),
        "mkdir -p \"$OUT_DIR\"".to_string(),
        "for f in src/*.rs; do".to_string(),
        format!("  echo \"compiling $f (job {n})\""),
        "  cp \"$f\" \"$OUT_DIR/\"".to_string(),
        "done".to_string(),
        String::new(),
        "if [ -d \"$OUT_DIR\" ]; then".to_string(),
        format!("  echo \"job {n} staged $(ls \"$OUT_DIR\" | wc -l) files\""),
        "else".to_string(),
        format!("  echo \"job {n} failed\" >&2"),
        "  exit 1".to_string(),
        "fi".to_string(),
    ];
    lines.join("\n") + "\n"
}

/// ~50 code fences across rust/python/js/toml/sh with realistic bodies
/// (~100 KB total) — exercises syntect across five syntaxes.
/// Language token, snippet generator.
type LangGenerator = (&'static str, fn(usize) -> String);

fn code_heavy() -> String {
    let generators: [LangGenerator; 5] = [
        ("rust", rust_snippet),
        ("python", python_snippet),
        ("js", js_snippet),
        ("toml", toml_snippet),
        ("sh", sh_snippet),
    ];
    let mut s = String::from("# Code-heavy document\n\n");
    for i in 0..50 {
        let (lang, generate) = generators[i % generators.len()];
        s.push_str(&format!(
            "## Snippet {}\n\n```{lang}\n{}```\n\n",
            i + 1,
            generate(i)
        ));
    }
    s
}

/// ~100 inline `$x^2$` formulas + 20 display `$$…$$` formulas.
fn math_heavy() -> String {
    let mut s = String::from("# Math-heavy document\n\n");
    for i in 0..100 {
        s.push_str(&format!(
            "Equation {i}: $x_{{{i}}}^2 + y_{{{i}}}^2 = z_{{{i}}}^2$ holds for the {i}th case.\n\n"
        ));
    }
    for i in 0..20 {
        s.push_str(&format!(
            "$$\\sum_{{n=1}}^{{{}}} \\frac{{1}}{{n^2}} = \\frac{{\\pi^2}}{{6}} - \\epsilon_{{{i}}}$$\n\n",
            i + 10
        ));
    }
    s
}

/// 5 small mermaid flowcharts — exercises merman.
fn mermaid() -> String {
    let mut s = String::from("# Mermaid document\n\n");
    for i in 0..5 {
        s.push_str(&format!(
            "```mermaid\nflowchart TD\n  A{i}[Start {i}] --> B{i}{{Decide}}\n\
             \x20 B{i} -->|yes| C{i}[Do it]\n  B{i} -->|no| D{i}[Skip]\n\
             \x20 C{i} --> E{i}[End]\n  D{i} --> E{i}\n```\n\n"
        ));
    }
    s
}

/// ~200 `[[Note N]]` links + 20 callouts — exercises the Obsidian passes.
fn wikilinks() -> String {
    let mut s = String::from("# Wikilinks document\n\n");
    for i in 0..200 {
        s.push_str(&format!("See [[Note {i}]] for details. "));
        if i % 8 == 7 {
            s.push_str("\n\n");
        }
    }
    s.push_str("\n\n");
    for i in 0..20 {
        s.push_str(&format!(
            "> [!tip]+ Callout {i}\n> Body referencing [[Note {i}]] with a little more text to pad it out.\n\n"
        ));
    }
    s
}

/// ~50 GFM tables, 10 rows x 5 columns each.
fn table_heavy() -> String {
    let mut s = String::from("# Table-heavy document\n\n");
    for t in 0..50 {
        s.push_str("| Col A | Col B | Col C | Col D | Col E |\n");
        s.push_str("|---|---|---|---|---|\n");
        for r in 0..10 {
            s.push_str(&format!(
                "| t{t}r{r}-a | t{t}r{r}-b | t{t}r{r}-c | t{t}r{r}-d | t{t}r{r}-e |\n"
            ));
        }
        s.push('\n');
    }
    s
}

const DEMO: &str = include_str!("../demo/demo.md");

// ---------------------------------------------------------------------------
// Benchmark
// ---------------------------------------------------------------------------

/// The document shapes under measurement, by name. One list for both the
/// criterion timing below and the instruction-count mode ([`render_once`]).
fn fixtures() -> Vec<(&'static str, String)> {
    vec![
        ("prose_10k", prose(10_000)),
        ("prose_500k", prose(500_000)),
        ("code_heavy", code_heavy()),
        ("math_heavy", math_heavy()),
        ("mermaid", mermaid()),
        ("wikilinks", wikilinks()),
        ("table_heavy", table_heavy()),
        ("demo", DEMO.to_string()),
    ]
}

fn bench_pipeline(c: &mut Criterion) {
    let vault = bench_vault();
    let opts = Options::default();
    let cases = fixtures();

    let mut group = c.benchmark_group("pipeline::render");
    for (name, md) in &cases {
        group.bench_function(*name, |b| {
            b.iter(|| pipeline::render(black_box(md), black_box(&opts), black_box(&vault)))
        });
    }
    group.finish();
}

/// `--once NAME [--repeat K]`: render fixture NAME K more times after one
/// warm-up render, then exit — no criterion, no timing.
///
/// This is the instruction-counting mode `scripts/bench-instructions.sh` runs
/// under valgrind's cachegrind. Wall-clock benches drift with CPU frequency
/// and runner type (measured: ±30% run to run on a shared runner, worse on a
/// throttling laptop); the instructions a render retires do not. Two runs,
/// K = 0 and K = N, differ by exactly N renders, so `(I_N − I_0) / N` is the
/// per-render count with process start-up and the one-time syntect/math init
/// (paid inside the warm-up render) cancelled out.
struct Once {
    fixture: String,
    repeat: usize,
}

fn once_args() -> Option<Once> {
    let args: Vec<String> = std::env::args().collect();
    let pos = args.iter().position(|a| a == "--once")?;
    let fixture = args.get(pos + 1)?.clone();
    let repeat = args
        .iter()
        .position(|a| a == "--repeat")
        .and_then(|p| args.get(p + 1))
        .and_then(|k| k.parse().ok())
        .unwrap_or(1);
    Some(Once { fixture, repeat })
}

fn render_once(once: &Once) {
    let vault = bench_vault();
    let opts = Options::default();
    let Some((_, md)) = fixtures().into_iter().find(|(n, _)| *n == once.fixture) else {
        eprintln!("unknown fixture {:?}", once.fixture);
        std::process::exit(2);
    };
    // Warm-up: the OnceLock initialisations land here, in both K runs alike.
    black_box(pipeline::render(black_box(&md), &opts, &vault));
    for _ in 0..once.repeat {
        black_box(pipeline::render(black_box(&md), &opts, &vault));
    }
}

fn main() {
    if let Some(once) = once_args() {
        render_once(&once);
        return;
    }

    // One-time cold cost (syntect's `OnceLock` syntax/theme load, math CSS/font
    // init) measured separately from the warm steady state Criterion reports.
    let vault = bench_vault();
    let opts = Options::default();
    let cold_md = "```rust\nfn f() {}\n```\n\nInline $x^2$ math.\n";

    let start = Instant::now();
    let _ = pipeline::render(cold_md, &opts, &vault);
    let cold = start.elapsed();

    let start = Instant::now();
    let _ = pipeline::render(cold_md, &opts, &vault);
    let warm = start.elapsed();

    eprintln!("cold first render: {cold:?}, warm: {warm:?}");

    let mut criterion = Criterion::default().configure_from_args();
    bench_pipeline(&mut criterion);
    criterion.final_summary();
}
