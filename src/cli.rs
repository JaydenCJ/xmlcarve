//! Argument parsing and command dispatch.
//!
//! Parsing is hand-rolled (zero dependencies) and split into pure functions
//! that return a command struct or an error string, so every flag
//! combination is unit-testable without spawning a process.

use std::fs::File;
use std::io::{self, Read, Write};
use std::process::ExitCode;

use crate::carve::{carve, CarveError, CarveOptions};
use crate::inspect::{inspect, InspectOptions};
use crate::record::MapOptions;
use crate::selector::Selector;
use crate::xml::ParserOptions;

const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "\
xmlcarve — stream giant XML files into JSONL by record element

USAGE:
    xmlcarve <COMMAND> [OPTIONS] <FILE>

COMMANDS:
    carve      Stream records matching a selector to JSONL
    inspect    Profile element structure and suggest a record selector

OPTIONS:
    -h, --help       Print help (or 'xmlcarve <COMMAND> --help')
    -V, --version    Print version
";

const CARVE_USAGE: &str = "\
Stream records matching a selector to JSONL, one line per record.

USAGE:
    xmlcarve carve [OPTIONS] --record <SELECTOR> <FILE>

ARGS:
    <FILE>    Input XML file, or '-' to read from stdin

OPTIONS:
    -r, --record <SELECTOR>    Record selector (repeatable). Forms:
                               'page' (any depth), 'feed/entry' (suffix),
                               '/root/items/item' (anchored), '*/row'
    -o, --output <FILE>        Write JSONL to a file instead of stdout
        --skip <N>             Skip the first N matching records
        --limit <N>            Stop after N records (stops reading early)
        --attr-prefix <S>      Attribute key prefix [default: @]
        --text-key <S>         Mixed-content text key [default: #text]
        --wrap                 Wrap each record as {\"<element>\": ...}
        --strip-namespaces     Strip 'ns:' prefixes and xmlns declarations
        --infer-types          Emit numeric/boolean-looking scalars as
                               JSON numbers and booleans
        --lenient              Pass unknown named entities (&nbsp;) through
        --stats                Print a summary line to stderr
    -h, --help                 Print help
";

const INSPECT_USAGE: &str = "\
Profile element structure and suggest a record selector.

USAGE:
    xmlcarve inspect [OPTIONS] <FILE>

ARGS:
    <FILE>    Input XML file, or '-' to read from stdin

OPTIONS:
        --limit <N>    Stop after scanning N elements (profile a giant
                       file from its first slice)
        --lenient      Pass unknown named entities (&nbsp;) through
    -h, --help         Print help
";

/// Parsed `carve` invocation.
#[derive(Debug, Clone, PartialEq)]
pub struct CarveCmd {
    pub file: String,
    pub output: Option<String>,
    pub opts: CarveOptions,
    pub stats: bool,
}

/// Parsed `inspect` invocation.
#[derive(Debug, Clone, PartialEq)]
pub struct InspectCmd {
    pub file: String,
    pub limit: Option<u64>,
    pub lenient: bool,
}

/// Entry point used by `main`. Returns the process exit code:
/// 0 success, 1 runtime failure, 2 usage error.
pub fn run(args: Vec<String>) -> ExitCode {
    match args.first().map(String::as_str) {
        None => {
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
        Some("-h" | "--help") => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some("-V" | "--version") => {
            println!("xmlcarve {VERSION}");
            ExitCode::SUCCESS
        }
        Some("carve") => match parse_carve_args(&args[1..]) {
            Ok(Some(cmd)) => run_carve(&cmd),
            Ok(None) => {
                print!("{CARVE_USAGE}");
                ExitCode::SUCCESS
            }
            Err(e) => usage_error(&e, "xmlcarve carve --help"),
        },
        Some("inspect") => match parse_inspect_args(&args[1..]) {
            Ok(Some(cmd)) => run_inspect(&cmd),
            Ok(None) => {
                print!("{INSPECT_USAGE}");
                ExitCode::SUCCESS
            }
            Err(e) => usage_error(&e, "xmlcarve inspect --help"),
        },
        Some(other) => usage_error(&format!("unknown command '{other}'"), "xmlcarve --help"),
    }
}

fn usage_error(message: &str, help: &str) -> ExitCode {
    eprintln!("xmlcarve: error: {message}");
    eprintln!("Run '{help}' for usage.");
    ExitCode::from(2)
}

/// Parse `carve` arguments. `Ok(None)` means `--help` was requested.
pub fn parse_carve_args(args: &[String]) -> Result<Option<CarveCmd>, String> {
    let mut selectors: Vec<Selector> = Vec::new();
    let mut file: Option<String> = None;
    let mut output: Option<String> = None;
    let mut map = MapOptions::default();
    let mut skip = 0u64;
    let mut limit: Option<u64> = None;
    let mut lenient = false;
    let mut stats = false;

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "-r" | "--record" => {
                let v = value_of(&mut it, arg)?;
                selectors.push(Selector::parse(&v).map_err(|e| format!("--record: {e}"))?);
            }
            "-o" | "--output" => output = Some(value_of(&mut it, arg)?),
            "--skip" => skip = number_of(&mut it, arg)?,
            "--limit" => limit = Some(number_of(&mut it, arg)?),
            "--attr-prefix" => map.attr_prefix = value_of(&mut it, arg)?,
            "--text-key" => {
                let v = value_of(&mut it, arg)?;
                if v.is_empty() {
                    return Err("--text-key must not be empty".to_string());
                }
                map.text_key = v;
            }
            "--wrap" => map.wrap = true,
            "--strip-namespaces" => map.strip_namespaces = true,
            "--infer-types" => map.infer_types = true,
            "--lenient" => lenient = true,
            "--stats" => stats = true,
            other if other.starts_with('-') && other != "-" => {
                return Err(format!("unknown option '{other}'"));
            }
            _ => {
                if file.is_some() {
                    return Err(format!("unexpected extra argument '{arg}'"));
                }
                file = Some(arg.clone());
            }
        }
    }
    let file = file.ok_or("missing input file (use '-' for stdin)")?;
    if selectors.is_empty() {
        return Err("at least one --record selector is required \
             (try 'xmlcarve inspect <FILE>' to find one)"
            .to_string());
    }
    if limit == Some(0) {
        return Err("--limit must be at least 1".to_string());
    }
    Ok(Some(CarveCmd {
        file,
        output,
        opts: CarveOptions {
            selectors,
            skip,
            limit,
            map,
            parser: ParserOptions {
                lenient_entities: lenient,
            },
        },
        stats,
    }))
}

/// Parse `inspect` arguments. `Ok(None)` means `--help` was requested.
pub fn parse_inspect_args(args: &[String]) -> Result<Option<InspectCmd>, String> {
    let mut file: Option<String> = None;
    let mut limit: Option<u64> = None;
    let mut lenient = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "--limit" => limit = Some(number_of(&mut it, arg)?),
            "--lenient" => lenient = true,
            other if other.starts_with('-') && other != "-" => {
                return Err(format!("unknown option '{other}'"));
            }
            _ => {
                if file.is_some() {
                    return Err(format!("unexpected extra argument '{arg}'"));
                }
                file = Some(arg.clone());
            }
        }
    }
    if limit == Some(0) {
        return Err("--limit must be at least 1".to_string());
    }
    let file = file.ok_or("missing input file (use '-' for stdin)")?;
    Ok(Some(InspectCmd {
        file,
        limit,
        lenient,
    }))
}

fn value_of<'a>(it: &mut std::slice::Iter<'a, String>, flag: &str) -> Result<String, String> {
    it.next()
        .cloned()
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn number_of(it: &mut std::slice::Iter<'_, String>, flag: &str) -> Result<u64, String> {
    let v = value_of(it, flag)?;
    v.parse::<u64>()
        .map_err(|_| format!("{flag} expects a non-negative integer, got '{v}'"))
}

/// Open the input: a file path or `-` for stdin.
fn open_input(path: &str) -> Result<Box<dyn Read>, String> {
    if path == "-" {
        Ok(Box::new(io::stdin().lock()))
    } else {
        File::open(path)
            .map(|f| Box::new(f) as Box<dyn Read>)
            .map_err(|e| format!("{path}: {e}"))
    }
}

fn run_carve(cmd: &CarveCmd) -> ExitCode {
    let input = match open_input(&cmd.file) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("xmlcarve: {e}");
            return ExitCode::FAILURE;
        }
    };
    let result = match &cmd.output {
        Some(path) => match File::create(path) {
            Ok(f) => {
                let mut w = io::BufWriter::new(f);
                carve(input, &mut w, &cmd.opts).and_then(|s| {
                    w.flush()?;
                    Ok(s)
                })
            }
            Err(e) => {
                eprintln!("xmlcarve: {path}: {e}");
                return ExitCode::FAILURE;
            }
        },
        None => {
            let stdout = io::stdout();
            let mut w = io::BufWriter::new(stdout.lock());
            carve(input, &mut w, &cmd.opts).and_then(|s| {
                w.flush()?;
                Ok(s)
            })
        }
    };
    match result {
        Ok(stats) => {
            if cmd.stats {
                eprintln!(
                    "xmlcarve: wrote {} record(s) ({} matched, {} skipped), {} bytes read",
                    stats.records_written,
                    stats.records_matched,
                    stats.records_matched - stats.records_written,
                    stats.bytes_read
                );
            }
            ExitCode::SUCCESS
        }
        // Downstream closed the pipe (e.g. `| head`): that is a consumer
        // decision, not a failure.
        Err(CarveError::Io(e)) if e.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xmlcarve: {}: {e}", cmd.file);
            ExitCode::FAILURE
        }
    }
}

fn run_inspect(cmd: &InspectCmd) -> ExitCode {
    let input = match open_input(&cmd.file) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("xmlcarve: {e}");
            return ExitCode::FAILURE;
        }
    };
    let opts = InspectOptions {
        limit: cmd.limit,
        parser: ParserOptions {
            lenient_entities: cmd.lenient,
        },
    };
    match inspect(input, &opts) {
        Ok(report) => {
            let mut out = String::new();
            out.push_str(&format!("{:>10}  {:>10}  path\n", "count", "w/attrs"));
            for pc in &report.paths {
                out.push_str(&format!(
                    "{:>10}  {:>10}  {}\n",
                    pc.count, pc.with_attrs, pc.path
                ));
            }
            out.push_str(&format!(
                "\n{} element(s) scanned, {} bytes read{}\n",
                report.elements_seen,
                report.bytes_read,
                if report.truncated {
                    " (scan truncated by --limit)"
                } else {
                    ""
                }
            ));
            match &report.suggestion {
                Some(s) => out.push_str(&format!("suggested --record: {s}\n")),
                None => out.push_str("no repeating record element found\n"),
            }
            print!("{out}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("xmlcarve: {}: {e}", cmd.file);
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    //! Flag parsing is pure; every rejection path a user can hit gets a
    //! precise error string they can act on.

    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn carve_requires_a_selector_and_a_file_with_actionable_errors() {
        let err = parse_carve_args(&argv(&["file.xml"])).unwrap_err();
        assert!(err.contains("--record"), "got: {err}");
        assert!(
            err.contains("inspect"),
            "should suggest the discovery path: {err}"
        );
        let err = parse_carve_args(&argv(&["-r", "page"])).unwrap_err();
        assert!(err.contains("missing input file"), "got: {err}");
        assert!(err.contains("stdin"), "should mention '-': {err}");
    }

    #[test]
    fn carve_parses_the_full_flag_set() {
        let cmd = parse_carve_args(&argv(&[
            "-r",
            "page",
            "--record",
            "entry",
            "--skip",
            "2",
            "--limit",
            "5",
            "--attr-prefix",
            "",
            "--text-key",
            "text",
            "--wrap",
            "--strip-namespaces",
            "--infer-types",
            "--lenient",
            "--stats",
            "-o",
            "out.jsonl",
            "dump.xml",
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(cmd.file, "dump.xml");
        assert_eq!(cmd.output.as_deref(), Some("out.jsonl"));
        assert_eq!(cmd.opts.selectors.len(), 2);
        assert_eq!(cmd.opts.skip, 2);
        assert_eq!(cmd.opts.limit, Some(5));
        assert_eq!(cmd.opts.map.attr_prefix, "");
        assert_eq!(cmd.opts.map.text_key, "text");
        assert!(cmd.opts.map.wrap);
        assert!(cmd.opts.map.strip_namespaces);
        assert!(cmd.opts.map.infer_types);
        assert!(cmd.opts.parser.lenient_entities);
        assert!(cmd.stats);
    }

    #[test]
    fn dash_means_stdin_and_is_not_an_unknown_option() {
        let cmd = parse_carve_args(&argv(&["-r", "page", "-"]))
            .unwrap()
            .unwrap();
        assert_eq!(cmd.file, "-");
    }

    #[test]
    fn numeric_flags_reject_garbage_with_the_offending_value() {
        // Bad selector values carry the flag context too.
        let err = parse_carve_args(&argv(&["-r", "a//b", "f.xml"])).unwrap_err();
        assert!(err.starts_with("--record:"), "got: {err}");
        let err = parse_carve_args(&argv(&["-r", "p", "--skip", "many", "f.xml"])).unwrap_err();
        assert!(err.contains("'many'"), "got: {err}");
        let err = parse_carve_args(&argv(&["-r", "p", "--limit"])).unwrap_err();
        assert!(err.contains("requires a value"), "got: {err}");
        let err = parse_carve_args(&argv(&["-r", "p", "--limit", "0", "f.xml"])).unwrap_err();
        assert!(err.contains("at least 1"), "got: {err}");
    }

    #[test]
    fn unknown_options_and_extra_positionals_are_rejected() {
        let err = parse_carve_args(&argv(&["-r", "p", "--frobnicate", "f.xml"])).unwrap_err();
        assert!(err.contains("--frobnicate"), "got: {err}");
        let err = parse_carve_args(&argv(&["-r", "p", "a.xml", "b.xml"])).unwrap_err();
        assert!(err.contains("'b.xml'"), "got: {err}");
    }

    #[test]
    fn empty_text_key_is_rejected_but_empty_attr_prefix_is_allowed() {
        // No prefix = attributes merge into the same namespace as children;
        // legitimate. An empty text key would produce {"": ...} — never.
        assert!(parse_carve_args(&argv(&["-r", "p", "--attr-prefix", "", "f.xml"])).is_ok());
        let err = parse_carve_args(&argv(&["-r", "p", "--text-key", "", "f.xml"])).unwrap_err();
        assert!(err.contains("--text-key"), "got: {err}");
    }

    #[test]
    fn help_flags_short_circuit_to_ok_none() {
        assert_eq!(parse_carve_args(&argv(&["--help"])).unwrap(), None);
        assert_eq!(
            parse_carve_args(&argv(&["-r", "p", "-h", "f.xml"])).unwrap(),
            None
        );
        assert_eq!(parse_inspect_args(&argv(&["--help"])).unwrap(), None);
    }

    #[test]
    fn inspect_parses_limit_lenient_and_file() {
        let cmd = parse_inspect_args(&argv(&["--limit", "1000", "--lenient", "dump.xml"]))
            .unwrap()
            .unwrap();
        assert_eq!(
            cmd,
            InspectCmd {
                file: "dump.xml".into(),
                limit: Some(1000),
                lenient: true
            }
        );
    }

    #[test]
    fn inspect_rejects_carve_only_flags() {
        let err = parse_inspect_args(&argv(&["--wrap", "f.xml"])).unwrap_err();
        assert!(err.contains("--wrap"), "got: {err}");
    }
}
