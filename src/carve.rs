//! The streaming driver: XML events in, JSONL lines out.
//!
//! Memory profile: one element-path stack (bounded by nesting depth) plus
//! the JSON tree of the *current* record. Everything between records — site
//! metadata, comments, indentation — flows past without being stored.

use std::fmt;
use std::io::{Read, Write};

use crate::record::{map_name, MapOptions, RecordBuilder};
use crate::selector::{any_match, Selector};
use crate::xml::{Event, ParseError, Parser, ParserOptions};

/// Everything `carve` needs to know, assembled by the CLI.
#[derive(Debug, Clone, PartialEq)]
pub struct CarveOptions {
    pub selectors: Vec<Selector>,
    /// Skip the first N matching records (they are parsed but not built).
    pub skip: u64,
    /// Stop after writing N records; parsing stops immediately after.
    pub limit: Option<u64>,
    pub map: MapOptions,
    pub parser: ParserOptions,
}

/// Counters reported by `--stats` and asserted by tests.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CarveStats {
    /// Records that matched a selector (written + skipped).
    pub records_matched: u64,
    /// Records actually written as JSONL lines.
    pub records_written: u64,
    /// Bytes consumed from the input (equals file size unless `--limit`
    /// stopped parsing early).
    pub bytes_read: u64,
}

/// Carve failure: either the XML was malformed or the output sink failed.
#[derive(Debug)]
pub enum CarveError {
    Parse(ParseError),
    Io(std::io::Error),
}

impl fmt::Display for CarveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CarveError::Parse(e) => write!(f, "{e}"),
            CarveError::Io(e) => write!(f, "output error: {e}"),
        }
    }
}

impl std::error::Error for CarveError {}

impl From<ParseError> for CarveError {
    fn from(e: ParseError) -> Self {
        CarveError::Parse(e)
    }
}

impl From<std::io::Error> for CarveError {
    fn from(e: std::io::Error) -> Self {
        CarveError::Io(e)
    }
}

/// What the driver is doing with the element currently being read.
enum State {
    /// Outside any record: watching the path for a selector match.
    Scanning,
    /// Inside a matched record that falls under `--skip`: just tracking
    /// depth, building nothing (cheap).
    Skipping { depth: usize },
    /// Inside a matched record: feeding the builder.
    Building(RecordBuilder),
}

/// Stream `input`, writing one JSON line per matched record to `out`.
pub fn carve<R: Read, W: Write>(
    input: R,
    out: &mut W,
    opts: &CarveOptions,
) -> Result<CarveStats, CarveError> {
    let mut parser = Parser::new(input, opts.parser.clone());
    let mut stats = CarveStats::default();
    let mut path: Vec<String> = Vec::new();
    let mut state = State::Scanning;

    loop {
        let event = parser.next_event()?;
        match event {
            Event::Start { name, attributes } => {
                path.push(map_name(&name, &opts.map));
                match &mut state {
                    State::Scanning => {
                        if any_match(&opts.selectors, &path) {
                            stats.records_matched += 1;
                            if stats.records_matched <= opts.skip {
                                state = State::Skipping { depth: path.len() };
                            } else {
                                let mut builder = RecordBuilder::new(opts.map.clone());
                                builder.start(&name, &attributes);
                                state = State::Building(builder);
                            }
                        }
                    }
                    State::Skipping { .. } => {}
                    State::Building(builder) => builder.start(&name, &attributes),
                }
            }
            Event::End { .. } => {
                match &mut state {
                    State::Scanning => {}
                    State::Skipping { depth } => {
                        if path.len() == *depth {
                            state = State::Scanning;
                        }
                    }
                    State::Building(builder) => {
                        if let Some(value) = builder.end() {
                            out.write_all(value.to_json().as_bytes())?;
                            out.write_all(b"\n")?;
                            stats.records_written += 1;
                            state = State::Scanning;
                            if opts.limit.is_some_and(|n| stats.records_written >= n) {
                                stats.bytes_read = parser.bytes_read();
                                return Ok(stats);
                            }
                        }
                    }
                }
                path.pop();
            }
            Event::Text(t) => {
                if let State::Building(builder) = &mut state {
                    builder.text(&t, false);
                }
            }
            Event::CData(t) => {
                if let State::Building(builder) = &mut state {
                    builder.text(&t, true);
                }
            }
            Event::Comment(_) | Event::Pi(_) | Event::Declaration | Event::Doctype => {}
            Event::Eof => break,
        }
    }
    stats.bytes_read = parser.bytes_read();
    Ok(stats)
}

#[cfg(test)]
mod tests {
    //! End-to-end through the library: XML string in, JSONL string out.
    //! These are the contracts the CLI integration tests re-verify through
    //! the real binary.

    use super::*;

    fn run(
        doc: &str,
        selectors: &[&str],
        tweak: impl FnOnce(&mut CarveOptions),
    ) -> (String, CarveStats) {
        let mut opts = CarveOptions {
            selectors: selectors
                .iter()
                .map(|s| Selector::parse(s).unwrap())
                .collect(),
            skip: 0,
            limit: None,
            map: MapOptions::default(),
            parser: ParserOptions::default(),
        };
        tweak(&mut opts);
        let mut out = Vec::new();
        let stats = carve(doc.as_bytes(), &mut out, &opts).expect("carve should succeed");
        (String::from_utf8(out).unwrap(), stats)
    }

    const WIKI: &str = r#"<mediawiki>
  <siteinfo><sitename>Testwiki</sitename></siteinfo>
  <page><title>Alpha</title><id>1</id></page>
  <page><title>Beta</title><id>2</id></page>
  <page><title>Gamma</title><id>3</id></page>
</mediawiki>"#;

    #[test]
    fn carves_one_json_line_per_matching_record() {
        let (out, stats) = run(WIKI, &["page"], |_| {});
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], r#"{"title":"Alpha","id":"1"}"#);
        assert_eq!(lines[2], r#"{"title":"Gamma","id":"3"}"#);
        assert_eq!(stats.records_matched, 3);
        assert_eq!(stats.records_written, 3);
        assert_eq!(stats.bytes_read, WIKI.len() as u64);
    }

    #[test]
    fn siteinfo_and_other_non_matching_elements_flow_past() {
        let (out, _) = run(WIKI, &["page"], |_| {});
        assert!(
            !out.contains("Testwiki"),
            "siteinfo must not leak into records"
        );
        // And zero matches is a success with zero lines, not an error.
        let (out, stats) = run(WIKI, &["nonexistent"], |_| {});
        assert_eq!(out, "");
        assert_eq!(stats.records_matched, 0);
        assert_eq!(stats.records_written, 0);
    }

    #[test]
    fn skip_drops_leading_records_without_building_them() {
        let (out, stats) = run(WIKI, &["page"], |o| o.skip = 2);
        assert_eq!(out, "{\"title\":\"Gamma\",\"id\":\"3\"}\n");
        assert_eq!(stats.records_matched, 3);
        assert_eq!(stats.records_written, 1);
    }

    #[test]
    fn limit_stops_parsing_early_and_composes_with_skip_into_a_window() {
        let (out, stats) = run(WIKI, &["page"], |o| o.limit = Some(1));
        assert_eq!(out, "{\"title\":\"Alpha\",\"id\":\"1\"}\n");
        assert_eq!(stats.records_written, 1);
        // Early exit is the point of --limit on a 40 GB file: we must not
        // have read to the end.
        assert!(
            stats.bytes_read < WIKI.len() as u64,
            "should stop before EOF"
        );

        let (out, stats) = run(WIKI, &["page"], |o| {
            o.skip = 1;
            o.limit = Some(1);
        });
        assert_eq!(out, "{\"title\":\"Beta\",\"id\":\"2\"}\n");
        assert_eq!(stats.records_matched, 2);
    }

    #[test]
    fn nested_matches_inside_a_record_do_not_start_new_records() {
        // <item> inside <item>: only the outermost becomes a record; the
        // inner one is a child field of it.
        let doc = "<r><item><name>outer</name><item><name>inner</name></item></item></r>";
        let (out, stats) = run(doc, &["item"], |_| {});
        assert_eq!(out, "{\"name\":\"outer\",\"item\":{\"name\":\"inner\"}}\n");
        assert_eq!(stats.records_matched, 1);
    }

    #[test]
    fn multiple_selectors_carve_a_union() {
        let doc = "<db><user><n>u1</n></user><group><n>g1</n></group><other/></db>";
        let (out, _) = run(doc, &["user", "group"], |_| {});
        assert_eq!(out, "{\"n\":\"u1\"}\n{\"n\":\"g1\"}\n");
    }

    #[test]
    fn anchored_selector_ignores_lookalikes_deeper_in_the_tree() {
        let doc = "<root><item>top</item><nested><item>deep</item></nested></root>";
        let (out, _) = run(doc, &["/root/item"], |_| {});
        assert_eq!(out, "\"top\"\n");
    }

    #[test]
    fn wrap_and_infer_flow_through_from_options() {
        let doc = "<db><row><id>7</id><ok>true</ok></row></db>";
        let (out, _) = run(doc, &["row"], |o| {
            o.map.wrap = true;
            o.map.infer_types = true;
        });
        assert_eq!(out, "{\"row\":{\"id\":7,\"ok\":true}}\n");
    }

    #[test]
    fn namespace_stripping_applies_to_selector_matching_too() {
        let doc = r#"<w:doc xmlns:w="http://example.test/w"><w:rec><w:v>1</w:v></w:rec></w:doc>"#;
        // With stripping on, the selector is written without the prefix.
        let (out, _) = run(doc, &["doc/rec"], |o| o.map.strip_namespaces = true);
        assert_eq!(out, "{\"v\":\"1\"}\n");
        // Without stripping, the prefixed name is what matches.
        let (out2, _) = run(doc, &["w:doc/w:rec"], |_| {});
        assert_eq!(out2, "{\"w:v\":\"1\"}\n");
    }

    #[test]
    fn rootless_fragment_streams_carve_fine() {
        // Log-style: many roots, no wrapper element at all.
        let doc = "<event id=\"1\"/>\n<event id=\"2\"/>\n";
        let (out, stats) = run(doc, &["event"], |_| {});
        assert_eq!(out, "{\"@id\":\"1\"}\n{\"@id\":\"2\"}\n");
        assert_eq!(stats.records_matched, 2);
    }

    #[test]
    fn record_content_with_entities_and_cdata_round_trips() {
        let doc = "<r><p><t>a &amp; b</t><body><![CDATA[<raw> & bytes]]></body></p></r>";
        let (out, _) = run(doc, &["p"], |_| {});
        assert_eq!(out, "{\"t\":\"a & b\",\"body\":\"<raw> & bytes\"}\n");
    }

    #[test]
    fn malformed_xml_surfaces_as_a_parse_error_with_line() {
        let doc = "<r>\n<page><title>ok</title></page>\n<page></oops>\n</r>";
        let opts = CarveOptions {
            selectors: vec![Selector::parse("page").unwrap()],
            skip: 0,
            limit: None,
            map: MapOptions::default(),
            parser: ParserOptions::default(),
        };
        let mut out = Vec::new();
        let err = carve(doc.as_bytes(), &mut out, &opts).unwrap_err();
        match err {
            CarveError::Parse(e) => assert_eq!(e.line, 3),
            other => panic!("expected parse error, got {other:?}"),
        }
        // The good record before the corruption was already emitted —
        // partial rescue is better than nothing.
        assert_eq!(String::from_utf8(out).unwrap(), "{\"title\":\"ok\"}\n");
    }

    #[test]
    fn limit_reached_before_corruption_never_sees_it() {
        // The whole point of streaming: damage past the carve window is
        // irrelevant.
        let doc = "<r><page><id>1</id></page><page></BROKEN";
        let opts = CarveOptions {
            selectors: vec![Selector::parse("page").unwrap()],
            skip: 0,
            limit: Some(1),
            map: MapOptions::default(),
            parser: ParserOptions::default(),
        };
        let mut out = Vec::new();
        let stats = carve(doc.as_bytes(), &mut out, &opts).expect("must not reach the damage");
        assert_eq!(stats.records_written, 1);
    }
}
