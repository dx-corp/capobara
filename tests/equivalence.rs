//! Runs `scripts/equivalence.sh` and requires a clean table.
//!
//! The harness clones ten public destination repositories, adds a detached
//! Mono worktree per source revision and builds a release binary in each, so
//! it is not part of the ordinary crate suite: it is `#[ignore]`d and the
//! source revisions must be named explicitly.
//!
//! ```sh
//! CAPOBARA_EQUIVALENCE_SOURCE_SHAS="<sha-a> <sha-b>" \
//!   cargo test --manifest-path rust/Cargo.toml --locked -p capobara \
//!   --test equivalence -- --ignored --nocapture
//! ```
//!
//! **This writes to the Mono checkout it is run from**: the script registers
//! a detached worktree per revision under that repository's `.git/worktrees/`
//! and removes them again in an `EXIT` trap. `CAPOBARA_EQUIVALENCE_KEEP=1`
//! suppresses the cleanup for debugging; this test never sets it, so a run
//! that fails still leaves the checkout as it found it.
//! `CAPOBARA_EQUIVALENCE_SCRATCH` chooses where the worktrees, clones and
//! per-run artefacts live; see the script's header for the rest.
//!
//! The script's own exit status is the assertion: it exits 1 if any row
//! differs in anything but the two sanctioned differences, the receipt's
//! `toolDigest` and the catalog matrix's `sourceSha` (see `EQUIVALENCE.md`).

use std::path::{Path, PathBuf};
use std::process::Command;

/// `rust/tools/capobara` -> the Mono checkout containing it. The harness
/// needs the checkout, not the crate: it reads `config/projections/` and
/// `scripts/projections/` from it and adds worktrees to it.
fn mono_root() -> PathBuf {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    crate_dir
        .ancestors()
        .nth(3)
        .expect("the crate lives at <mono>/rust/tools/capobara")
        .to_path_buf()
}

/// The data rows of the table whose header line starts with `header`, i.e.
/// every `| ` line after that header and its `| --- |` separator, stopping at
/// the first line that is not a row. The two tables are scoped separately on
/// purpose: catalog rows also contain `| <sha12> |`, so counting `| {short} |`
/// across the whole output silently mixes them into the projection count.
fn rows_of<'a>(table: &'a str, header: &str) -> Vec<&'a str> {
    table
        .lines()
        .skip_while(|line| !line.starts_with(header))
        .skip(2)
        .take_while(|line| line.starts_with("| "))
        .collect()
}

fn projection_rows(table: &str) -> Vec<&str> {
    rows_of(table, "| projection | sha |")
}

fn catalog_rows(table: &str) -> Vec<&str> {
    rows_of(table, "| sha | catalog command |")
}

/// One `key=value` field of the script's `EQUIVALENCE-SUMMARY` line.
fn summary_field(table: &str, key: &str) -> usize {
    let line = table
        .lines()
        .find(|line| line.starts_with("EQUIVALENCE-SUMMARY "))
        .unwrap_or_else(|| panic!("no EQUIVALENCE-SUMMARY line in:\n{table}"));
    line.split_whitespace()
        .find_map(|field| field.strip_prefix(&format!("{key}=")))
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("no numeric {key} in: {line}"))
}

/// Checks one run's output against its own `EQUIVALENCE-SUMMARY` line. Shared
/// by the live harness test below and by the recorded-output test, so the
/// counting the gate depends on is exercised by the ordinary crate suite and
/// cannot rot while the harness test stays `#[ignore]`d.
fn check_table(table: &str, revisions: &[&str]) {
    let projections = summary_field(table, "projections");
    let cases = summary_field(table, "cases");
    let rows = projection_rows(table);
    assert_eq!(
        rows.len(),
        summary_field(table, "rows"),
        "projection table has {} rows, summary says {}\n{table}",
        rows.len(),
        summary_field(table, "rows")
    );
    assert_eq!(rows.len(), projections * cases * revisions.len(), "{table}");
    assert_eq!(
        catalog_rows(table).len(),
        summary_field(table, "catalog_rows"),
        "{table}"
    );

    // Each revision must occupy its own share of the PROJECTION rows, so a run
    // that projected one revision twice cannot masquerade as a run of two.
    for revision in revisions {
        let short = &revision[..12];
        let mine = rows
            .iter()
            .filter(|line| line.contains(&format!("| {short} |")))
            .count();
        assert_eq!(
            mine,
            projections * cases,
            "revision {short} occupies {mine} projection rows, expected {}\n{table}",
            projections * cases
        );
    }
}

/// Fenced blocks of `EQUIVALENCE.md` that hold a recorded harness run.
fn recorded_runs(markdown: &str) -> Vec<String> {
    let mut runs = Vec::new();
    let mut current: Option<Vec<&str>> = None;
    for line in markdown.lines() {
        if line.starts_with("```") {
            match current.take() {
                Some(block) => {
                    let text = block.join("\n");
                    if text.contains("EQUIVALENCE-SUMMARY ") {
                        runs.push(text);
                    }
                }
                None => current = Some(Vec::new()),
            }
        } else if let Some(block) = current.as_mut() {
            block.push(line);
        }
    }
    runs
}

/// The row counting the live gate relies on, run against the real recorded
/// output committed in `EQUIVALENCE.md`. This is the regression guard: the
/// live assertion is unreachable until a run goes fully green (the exit-status
/// assert fires first), so without this the counting could be wrong for months
/// and surface only at cutover -- which is exactly how it was wrong before.
#[test]
// `EQUIVALENCE.md` is Mono-internal and deliberately not projected (see
// `tests/definition_coverage.rs`'s UNPROJECTED allowlist), but this file IS
// projected -- `config/projections/capobara.json` includes `tests/**`. Without
// this gate the published `dx-corp/capobara` would carry a test that reads a
// file its tree does not contain, and `cargo test` there would fail on a fresh
// clone against a destination whose `main` requires a `ci` check.
//
// Gating rather than excluding the file: `Cargo.toml` is projected verbatim and
// declares `[[test]] name = "equivalence"` with an explicit `path`, and the
// crate sets `autotests = false`. Removing only the file leaves that entry
// dangling, and `cargo test` then fails with `can't find integration-test
// 'equivalence'` -- the same outcome by a different route. Both were measured;
// see the task-17 fix-round-2 report.
#[cfg_attr(
    not(feature = "mono-fixtures"),
    ignore = "reads EQUIVALENCE.md, which exists only inside dx-corp/mono"
)]
fn the_recorded_tables_satisfy_the_assertions_the_live_gate_makes() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("EQUIVALENCE.md");
    let markdown = std::fs::read_to_string(&path).expect("EQUIVALENCE.md is committed");
    let runs = recorded_runs(&markdown);
    assert_eq!(
        runs.len(),
        2,
        "expected two recorded runs in EQUIVALENCE.md"
    );

    for run in &runs {
        // The revisions of a recorded run are whichever short SHAs its
        // projection rows name, in first-seen order.
        let mut revisions: Vec<&str> = Vec::new();
        for row in projection_rows(run) {
            let short = row.split('|').nth(2).map(str::trim).unwrap_or_default();
            if short.len() == 12 && !revisions.contains(&short) {
                revisions.push(short);
            }
        }
        assert_eq!(revisions.len(), 2, "expected two revisions in:\n{run}");
        check_table(run, &revisions);

        // A recorded run must also have catalog rows, or the catalog gate was
        // not exercised when the table was taken.
        assert!(catalog_rows(run).len() >= 2, "{run}");
    }

    // Positive control: the counting is discriminating, not vacuous. Dropping
    // one projection row must break it. Without this, every assertion above
    // could be trivially satisfiable and the test would still pass.
    let dropped = "| private-runner | 3abd945b72f4 |";
    let mutilated: String = runs[0]
        .lines()
        .filter(|line| !line.starts_with(dropped))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        mutilated.lines().count() + 1,
        runs[0].lines().count(),
        "the control removed {} lines, expected exactly 1",
        runs[0].lines().count() - mutilated.lines().count()
    );
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let caught =
        std::panic::catch_unwind(|| check_table(&mutilated, &["3abd945b72f4", "7d84ae1d5871"]));
    std::panic::set_hook(previous);
    assert!(
        caught.is_err(),
        "check_table accepted a table with a projection row removed"
    );
}

#[test]
#[ignore = "clones ten repositories and builds a release binary per source revision; \
            set CAPOBARA_EQUIVALENCE_SOURCE_SHAS and run with --ignored"]
fn node_and_capobara_apply_produce_identical_trees_and_reports() {
    let shas = std::env::var("CAPOBARA_EQUIVALENCE_SOURCE_SHAS").unwrap_or_default();
    let shas: Vec<&str> = shas.split_whitespace().collect();
    assert_eq!(
        shas.len(),
        2,
        "set CAPOBARA_EQUIVALENCE_SOURCE_SHAS=\"<sha-a> <sha-b>\" to two full Mono commit SHAs"
    );

    let root = mono_root();
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/equivalence.sh");
    assert!(script.is_file(), "missing {}", script.display());

    #[allow(
        clippy::disallowed_methods,
        reason = "integration test executes the equivalence harness"
    )]
    let mut command = Command::new("bash");
    let output = command
        .arg(&script)
        .arg(&root)
        .arg(shas[0])
        .arg(shas[1])
        .output()
        .expect("failed to run scripts/equivalence.sh");

    let table = String::from_utf8_lossy(&output.stdout);
    println!("{table}");
    assert!(
        output.status.success(),
        "equivalence harness reported an unsanctioned difference\n{table}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Positive controls. A harness that produced no rows, or ran one revision
    // twice, would also exit 0; neither may pass. `check_table` scopes the
    // counting to the projection table -- catalog rows carry `| <sha12> |`
    // too -- and is the same function `the_recorded_tables_...` exercises
    // against the committed output.
    let projections: usize =
        std::fs::read_to_string(root.join("config/projections/repositories.json"))
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .and_then(|value| value["projections"].as_array().map(Vec::len))
            .expect("config/projections/repositories.json lists the catalog");
    assert_eq!(summary_field(&table, "projections"), projections, "{table}");
    assert_eq!(summary_field(&table, "failures"), 0, "{table}");
    assert_eq!(summary_field(&table, "catalog_failures"), 0, "{table}");
    assert_eq!(
        catalog_rows(table.as_ref()).len(),
        2 * shas.len(),
        "{table}"
    );
    check_table(table.as_ref(), &shas);
}
