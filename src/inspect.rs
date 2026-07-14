//! Structure profiler: answers "what is the record element of this file?"
//!
//! One streaming pass counts every distinct element *path* (memory bounded
//! by the schema, not the file) and applies a heuristic to suggest a
//! `--record` selector: the path that repeats most under a parent that does
//! not — i.e. the row of the table, the page of the wiki, the entry of the
//! feed.

use std::collections::HashMap;
use std::io::Read;

use crate::xml::{Event, ParseError, Parser, ParserOptions};

/// Profiler configuration.
#[derive(Debug, Clone, Default)]
pub struct InspectOptions {
    /// Stop after this many start elements — enough to profile a
    /// multi-gigabyte file from its first slice.
    pub limit: Option<u64>,
    pub parser: ParserOptions,
}

/// Occurrence counts for one distinct element path.
#[derive(Debug, Clone, PartialEq)]
pub struct PathCount {
    /// Slash-joined element path from the root, e.g. `mediawiki/page/title`.
    pub path: String,
    pub count: u64,
    /// Element occurrences (of this path) that carried attributes.
    pub with_attrs: u64,
}

/// The full profile of a document.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    /// Distinct paths in first-appearance (document) order.
    pub paths: Vec<PathCount>,
    pub elements_seen: u64,
    pub bytes_read: u64,
    /// True when `limit` stopped the scan before EOF.
    pub truncated: bool,
    /// Suggested `--record` selector, if the heuristic found a repeating
    /// record shape.
    pub suggestion: Option<String>,
}

/// Profile the document in one streaming pass.
pub fn inspect<R: Read>(input: R, opts: &InspectOptions) -> Result<Report, ParseError> {
    let mut parser = Parser::new(input, opts.parser.clone());
    let mut order: Vec<String> = Vec::new();
    let mut counts: HashMap<String, (u64, u64)> = HashMap::new();
    let mut path = String::new();
    let mut lengths: Vec<usize> = Vec::new(); // path length before each push
    let mut elements_seen = 0u64;
    let mut truncated = false;

    loop {
        match parser.next_event()? {
            Event::Start { name, attributes } => {
                lengths.push(path.len());
                if !path.is_empty() {
                    path.push('/');
                }
                path.push_str(&name);
                let entry = counts.entry(path.clone()).or_insert_with(|| {
                    order.push(path.clone());
                    (0, 0)
                });
                entry.0 += 1;
                if !attributes.is_empty() {
                    entry.1 += 1;
                }
                elements_seen += 1;
                if opts.limit.is_some_and(|n| elements_seen >= n) {
                    truncated = true;
                    break;
                }
            }
            Event::End { .. } => {
                let len = lengths.pop().unwrap_or(0);
                path.truncate(len);
            }
            Event::Eof => break,
            _ => {}
        }
    }

    let paths: Vec<PathCount> = order
        .into_iter()
        .map(|p| {
            let (count, with_attrs) = counts[&p];
            PathCount {
                path: p,
                count,
                with_attrs,
            }
        })
        .collect();
    let suggestion = suggest(&paths);
    Ok(Report {
        paths,
        elements_seen,
        bytes_read: parser.bytes_read(),
        truncated,
        suggestion,
    })
}

/// Pick the most likely record element: among paths that occur at least
/// twice and more often than their parent, prefer the *shallowest* (records
/// sit near the root; repeated leaves like `<link>` sit deep inside them),
/// then the highest count, then document order. A singleton is a header,
/// not a record, so it never qualifies.
fn suggest(paths: &[PathCount]) -> Option<String> {
    let count_of = |p: &str| {
        paths
            .iter()
            .find(|pc| pc.path == p)
            .map_or(0, |pc| pc.count)
    };
    let mut best: Option<(&PathCount, usize)> = None;
    for pc in paths {
        if pc.count < 2 {
            continue;
        }
        let parent_count = match pc.path.rsplit_once('/') {
            Some((parent, _)) => count_of(parent),
            None => 1, // document level
        };
        if pc.count <= parent_count {
            continue;
        }
        let depth = pc.path.matches('/').count();
        let better = match best {
            None => true,
            Some((b, bd)) => depth < bd || (depth == bd && pc.count > b.count),
        };
        if better {
            best = Some((pc, depth));
        }
    }
    best.map(|(pc, _)| pc.path.clone())
}

#[cfg(test)]
mod tests {
    //! The suggestion heuristic gets fixture shapes modeled on real dumps:
    //! wiki exports, feeds, and flat log streams.

    use super::*;

    fn report(doc: &str) -> Report {
        inspect(doc.as_bytes(), &InspectOptions::default()).expect("inspect should succeed")
    }

    #[test]
    fn counts_every_distinct_path_in_document_order() {
        let doc = "<r><a><b/></a><a><b/><b/></a></r>";
        let rep = report(doc);
        let as_pairs: Vec<(&str, u64)> = rep
            .paths
            .iter()
            .map(|p| (p.path.as_str(), p.count))
            .collect();
        assert_eq!(as_pairs, vec![("r", 1), ("r/a", 2), ("r/a/b", 3)]);
        assert_eq!(rep.elements_seen, 6);
        assert!(!rep.truncated);
        // Attribute-bearing occurrences are tallied separately.
        let rep = report("<r><i k=\"1\"/><i/><i k=\"2\"/></r>");
        let i = rep.paths.iter().find(|p| p.path == "r/i").unwrap();
        assert_eq!(i.count, 3);
        assert_eq!(i.with_attrs, 2);
    }

    #[test]
    fn wiki_shaped_dump_suggests_the_page_path() {
        let doc = "<mediawiki><siteinfo><sitename>W</sitename></siteinfo>\
                   <page><title>A</title></page><page><title>B</title></page></mediawiki>";
        assert_eq!(report(doc).suggestion.as_deref(), Some("mediawiki/page"));
    }

    #[test]
    fn repeated_leaves_do_not_beat_the_record_element() {
        // Each page holds 3 links, so <l> occurs 6 times vs 2 pages — but a
        // record element sits near the root; the shallower repeating path
        // must win over the noisier deep one.
        let doc = "<w><page><l/><l/><l/></page><page><l/><l/><l/></page></w>";
        assert_eq!(report(doc).suggestion.as_deref(), Some("w/page"));
    }

    #[test]
    fn equal_depth_candidates_are_ranked_by_count() {
        // A repeated header pair must not outrank the fiftyfold row —
        // at equal depth the higher count is the record.
        let mut doc = String::from("<r><h><k/><k/></h><d>");
        for _ in 0..50 {
            doc.push_str("<row/>");
        }
        doc.push_str("</d></r>");
        assert_eq!(report(&doc).suggestion.as_deref(), Some("r/d/row"));
    }

    #[test]
    fn rootless_event_stream_suggests_the_repeated_root() {
        let doc = "<event id=\"1\"/><event id=\"2\"/><event id=\"3\"/>";
        assert_eq!(report(doc).suggestion.as_deref(), Some("event"));
    }

    #[test]
    fn single_record_documents_yield_no_suggestion() {
        // Nothing repeats: there is no "record element" to find, and a wrong
        // guess would be worse than none.
        let doc = "<config><host>a</host><port>1</port></config>";
        assert_eq!(report(doc).suggestion, None);
    }

    #[test]
    fn limit_truncates_the_scan_and_reports_it() {
        let doc = "<r><a/><a/><a/><a/><a/></r>";
        let rep = inspect(
            doc.as_bytes(),
            &InspectOptions {
                limit: Some(3),
                ..InspectOptions::default()
            },
        )
        .unwrap();
        assert!(rep.truncated);
        assert_eq!(rep.elements_seen, 3);
        assert!(rep.bytes_read < doc.len() as u64);
        // Counts reflect what was seen, not the whole file.
        assert_eq!(rep.paths.iter().find(|p| p.path == "r/a").unwrap().count, 2);
    }

    #[test]
    fn deep_tie_breaks_prefer_the_shallower_path() {
        // Both b and c occur twice more than their parents; same count.
        // The shallower one is the better record candidate.
        let doc = "<r><b><c/></b><b><c/></b></r>";
        assert_eq!(report(doc).suggestion.as_deref(), Some("r/b"));
    }

    #[test]
    fn parse_errors_propagate_with_line_numbers() {
        let err = inspect("<a>\n</b>".as_bytes(), &InspectOptions::default()).unwrap_err();
        assert_eq!(err.line, 2);
    }
}
