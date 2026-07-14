//! End-to-end tests against the compiled `xmlcarve` binary.
//!
//! Each test writes its fixtures into its own temp directory (removed on
//! drop), runs the real binary via `CARGO_BIN_EXE_xmlcarve`, and asserts on
//! stdout/stderr/exit codes. Everything is offline and deterministic.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_xmlcarve");

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A per-test scratch directory, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> TempDir {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("xmlcarve-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn file(&self, name: &str, content: &str) -> PathBuf {
        let p = self.0.join(name);
        std::fs::write(&p, content).unwrap();
        p
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .output()
        .expect("binary should run")
}

fn run_stdin(args: &[&str], input: &str) -> Output {
    use std::io::Write as _;
    let mut child = Command::new(BIN)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary should spawn");
    // Feed stdin from a thread while the main thread drains stdout —
    // writing a large input without draining would deadlock on the pipe.
    let mut stdin = child.stdin.take().unwrap();
    let payload = input.to_string();
    let feeder = std::thread::spawn(move || {
        let _ = stdin.write_all(payload.as_bytes());
    });
    let out = child.wait_with_output().expect("binary should finish");
    feeder.join().expect("stdin feeder should not panic");
    out
}

fn stdout(o: &Output) -> String {
    String::from_utf8(o.stdout.clone()).unwrap()
}

fn stderr(o: &Output) -> String {
    String::from_utf8(o.stderr.clone()).unwrap()
}

const WIKI: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<mediawiki>
  <siteinfo><sitename>Testwiki</sitename></siteinfo>
  <page>
    <title>Alpha</title>
    <id>1</id>
    <revision><text bytes="12">Alpha body</text></revision>
  </page>
  <page>
    <title>Beta &amp; more</title>
    <id>2</id>
    <revision><text bytes="9">Beta body</text></revision>
  </page>
</mediawiki>
"#;

#[test]
fn version_and_help_report_the_package_metadata() {
    let v = run(&["--version"]);
    assert!(v.status.success());
    assert_eq!(
        stdout(&v).trim(),
        format!("xmlcarve {}", env!("CARGO_PKG_VERSION"))
    );

    let h = run(&["--help"]);
    assert!(h.status.success());
    let text = stdout(&h);
    assert!(text.contains("COMMANDS:"), "top help lists commands");
    assert!(text.contains("carve") && text.contains("inspect"));

    let ch = run(&["carve", "--help"]);
    assert!(ch.status.success());
    assert!(
        stdout(&ch).contains("--record"),
        "carve help documents --record"
    );
}

#[test]
fn carve_writes_one_json_line_per_record_to_stdout() {
    let dir = TempDir::new();
    let file = dir.file("wiki.xml", WIKI);
    let out = run(&["carve", "-r", "page", file.to_str().unwrap()]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let lines: Vec<String> = stdout(&out).lines().map(str::to_string).collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(
        lines[0],
        r##"{"title":"Alpha","id":"1","revision":{"text":{"@bytes":"12","#text":"Alpha body"}}}"##
    );
    assert!(
        lines[1].contains(r#""title":"Beta & more""#),
        "entities decode: {}",
        lines[1]
    );
}

#[test]
fn carve_reads_stdin_when_file_is_dash() {
    let out = run_stdin(&["carve", "-r", "page", "-"], WIKI);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out).lines().count(), 2);
}

#[test]
fn output_flag_writes_the_file_and_stats_go_to_stderr() {
    let dir = TempDir::new();
    let file = dir.file("wiki.xml", WIKI);
    let dest = dir.path().join("out.jsonl");
    let out = run(&[
        "carve",
        "-r",
        "page",
        "--stats",
        "-o",
        dest.to_str().unwrap(),
        file.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "", "records must not leak to stdout with -o");
    let written = std::fs::read_to_string(&dest).unwrap();
    assert_eq!(written.lines().count(), 2);
    let s = stderr(&out);
    assert!(s.contains("wrote 2 record(s)"), "stats line present: {s}");
    assert!(
        s.contains(&format!("{} bytes read", WIKI.len())),
        "byte count exact: {s}"
    );
}

#[test]
fn skip_limit_wrap_and_infer_types_shape_the_output() {
    let dir = TempDir::new();
    let file = dir.file("wiki.xml", WIKI);
    let out = run(&[
        "carve",
        "-r",
        "page",
        "--skip",
        "1",
        "--limit",
        "1",
        "--wrap",
        "--infer-types",
        file.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    assert_eq!(text.lines().count(), 1);
    assert!(
        text.starts_with(r#"{"page":"#),
        "--wrap nests under the element name: {text}"
    );
    assert!(
        text.contains(r#""id":2"#),
        "--infer-types makes the id numeric: {text}"
    );
}

#[test]
fn inspect_profiles_structure_and_suggests_the_record_selector() {
    let dir = TempDir::new();
    let file = dir.file("wiki.xml", WIKI);
    let out = run(&["inspect", file.to_str().unwrap()]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.contains("mediawiki/page/title"),
        "paths listed: {text}"
    );
    assert!(
        text.contains("suggested --record: mediawiki/page"),
        "suggestion: {text}"
    );
    assert!(
        text.contains("13 element(s) scanned"),
        "element count: {text}"
    );
}

#[test]
fn malformed_xml_fails_with_exit_1_line_number_and_partial_output() {
    let dir = TempDir::new();
    let file = dir.file(
        "broken.xml",
        "<r>\n<page><id>1</id></page>\n<page></oops>\n</r>\n",
    );
    let out = run(&["carve", "-r", "page", file.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    // The record before the damage was already streamed out — partial
    // rescue is the tool's promise.
    assert_eq!(stdout(&out), "{\"id\":\"1\"}\n");
    let s = stderr(&out);
    assert!(s.contains("line 3"), "error names the line: {s}");
    assert!(s.contains("broken.xml"), "error names the file: {s}");
}

#[test]
fn usage_errors_exit_2_with_guidance() {
    // No selector: points the user at inspect.
    let dir = TempDir::new();
    let file = dir.file("wiki.xml", WIKI);
    let out = run(&["carve", file.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr(&out).contains("inspect"),
        "guidance: {}",
        stderr(&out)
    );

    // Unknown command and unknown flag are usage errors too.
    assert_eq!(run(&["chop", "x.xml"]).status.code(), Some(2));
    assert_eq!(
        run(&["carve", "-r", "p", "--bogus", "x.xml"]).status.code(),
        Some(2)
    );

    // Missing file is a runtime error (exit 1), not a usage error.
    let out = run(&[
        "carve",
        "-r",
        "page",
        dir.path().join("nope.xml").to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("nope.xml"),
        "names the path: {}",
        stderr(&out)
    );
}

#[test]
fn lenient_flag_rescues_html_entity_dumps() {
    let doc = "<log><e>caf&eacute;</e><e>ok</e></log>";
    let strict = run_stdin(&["carve", "-r", "e", "-"], doc);
    assert_eq!(strict.status.code(), Some(1));
    assert!(
        stderr(&strict).contains("&eacute;"),
        "names the entity: {}",
        stderr(&strict)
    );

    let lenient = run_stdin(&["carve", "-r", "e", "--lenient", "-"], doc);
    assert!(lenient.status.success());
    assert_eq!(stdout(&lenient), "\"caf&eacute;\"\n\"ok\"\n");
}

#[test]
fn large_synthetic_dump_streams_completely_and_in_order() {
    // 2000 records with distinctive first/last markers: catches truncation,
    // reordering and buffer-boundary bugs at a size that still runs fast.
    let mut doc = String::from("<db>");
    for i in 0..2000 {
        doc.push_str(&format!(
            "<row><n>{i}</n><payload>{}</payload></row>",
            "x".repeat(200)
        ));
    }
    doc.push_str("</db>");
    let out = run_stdin(&["carve", "-r", "row", "--infer-types", "-"], &doc);
    assert!(out.status.success());
    let text = stdout(&out);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2000);
    assert!(lines[0].starts_with(r#"{"n":0,"#));
    assert!(lines[1999].starts_with(r#"{"n":1999,"#));
}
