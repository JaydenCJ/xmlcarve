//! Record-element selectors: which subtrees become JSONL records.
//!
//! The grammar is deliberately tiny — `/`-separated element names with `*`
//! wildcards — because picking the record element of a dump should never
//! require learning XPath:
//!
//! - `page` — any element named `page`, at any depth.
//! - `mediawiki/page` — a `page` whose direct parent is `mediawiki`
//!   (relative: matched as a suffix of the element path).
//! - `/root/items/item` — anchored at the document root (leading `/`).
//! - `*/entry` — an `entry` under any parent.

use std::fmt;

/// A parsed selector: match kind plus name segments.
#[derive(Debug, Clone, PartialEq)]
pub struct Selector {
    segments: Vec<Segment>,
    anchored: bool,
    source: String,
}

#[derive(Debug, Clone, PartialEq)]
enum Segment {
    Name(String),
    Wildcard,
}

impl fmt::Display for Selector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.source)
    }
}

impl Selector {
    /// Parse a selector string. Errors are meant for direct CLI display.
    pub fn parse(input: &str) -> Result<Selector, String> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err("selector is empty".to_string());
        }
        let (anchored, body) = match trimmed.strip_prefix('/') {
            Some(rest) => (true, rest),
            None => (false, trimmed),
        };
        if body.is_empty() {
            return Err(format!("selector '{trimmed}' has no element names"));
        }
        let mut segments = Vec::new();
        for part in body.split('/') {
            let part = part.trim();
            if part.is_empty() {
                return Err(format!(
                    "selector '{trimmed}' contains an empty segment ('//')"
                ));
            }
            if part == "*" {
                segments.push(Segment::Wildcard);
            } else if part.contains(['*', '[', ']', '@']) {
                return Err(format!(
                    "selector segment '{part}' is not supported: use plain element names and '*' \
                     (full XPath predicates are out of scope)"
                ));
            } else {
                segments.push(Segment::Name(part.to_string()));
            }
        }
        if anchored && segments.iter().all(|s| *s == Segment::Wildcard) {
            return Err(format!(
                "selector '{trimmed}' matches every element; anchor it with at least one name"
            ));
        }
        Ok(Selector {
            segments,
            anchored,
            source: trimmed.to_string(),
        })
    }

    /// Does the current element path (root first, current element last)
    /// select a record rooted at the last path entry?
    pub fn matches(&self, path: &[String]) -> bool {
        if self.anchored {
            self.segments.len() == path.len() && self.tail_matches(path)
        } else {
            self.segments.len() <= path.len() && self.tail_matches(path)
        }
    }

    fn tail_matches(&self, path: &[String]) -> bool {
        let tail = &path[path.len() - self.segments.len()..];
        self.segments.iter().zip(tail).all(|(seg, name)| match seg {
            Segment::Wildcard => true,
            Segment::Name(n) => n == name,
        })
    }
}

/// True if any selector in the set matches the path.
pub fn any_match(selectors: &[Selector], path: &[String]) -> bool {
    selectors.iter().any(|s| s.matches(path))
}

#[cfg(test)]
mod tests {
    //! Matching is over the element *path stack*, so tests build paths the
    //! same way the carve driver does: one name per open element.

    use super::*;

    fn path(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn bare_name_matches_at_any_depth() {
        let s = Selector::parse("page").unwrap();
        assert!(s.matches(&path(&["page"])));
        assert!(s.matches(&path(&["mediawiki", "page"])));
        assert!(s.matches(&path(&["a", "b", "c", "page"])));
        assert!(!s.matches(&path(&["pages"])));
        assert!(!s.matches(&path(&["page", "title"])));
    }

    #[test]
    fn relative_path_matches_as_a_suffix() {
        let s = Selector::parse("mediawiki/page").unwrap();
        assert!(s.matches(&path(&["mediawiki", "page"])));
        assert!(s.matches(&path(&["export", "mediawiki", "page"])));
        assert!(!s.matches(&path(&["mediawiki", "siteinfo", "page"])));
        // A suffix longer than the path can never match (and must not panic).
        assert!(!s.matches(&path(&["page"])));
    }

    #[test]
    fn anchored_path_matches_only_from_the_root() {
        let s = Selector::parse("/root/items/item").unwrap();
        assert!(s.matches(&path(&["root", "items", "item"])));
        assert!(!s.matches(&path(&["outer", "root", "items", "item"])));
        assert!(!s.matches(&path(&["root", "items"])));
    }

    #[test]
    fn wildcard_segment_matches_any_single_name() {
        let s = Selector::parse("*/entry").unwrap();
        assert!(s.matches(&path(&["feed", "entry"])));
        assert!(s.matches(&path(&["a", "b", "entry"])));
        // The wildcard still requires *some* parent: a root-level entry has
        // no element above it.
        assert!(!s.matches(&path(&["entry"])));
        // A lone unanchored "*" is legal — it means "every outermost
        // element", useful with --limit for eyeballing unfamiliar files.
        let all = Selector::parse("*").unwrap();
        assert!(all.matches(&path(&["anything"])));
        assert!(all.matches(&path(&["a", "b"])));
    }

    #[test]
    fn anchored_wildcard_pins_the_depth() {
        let s = Selector::parse("/*/row").unwrap();
        assert!(s.matches(&path(&["table", "row"])));
        assert!(!s.matches(&path(&["db", "table", "row"])));
    }

    #[test]
    fn surrounding_whitespace_is_tolerated() {
        let s = Selector::parse("  feed/entry ").unwrap();
        assert!(s.matches(&path(&["feed", "entry"])));
        assert_eq!(s.to_string(), "feed/entry");
    }

    #[test]
    fn degenerate_and_xpath_flavored_selectors_are_rejected_with_reasons() {
        assert!(Selector::parse("").unwrap_err().contains("empty"));
        assert!(Selector::parse("   ").unwrap_err().contains("empty"));
        assert!(Selector::parse("/")
            .unwrap_err()
            .contains("no element names"));
        assert!(Selector::parse("a//b")
            .unwrap_err()
            .contains("empty segment"));
        // "/*" would emit every element as a record — always a user mistake.
        assert!(Selector::parse("/*").unwrap_err().contains("every element"));
        let err = Selector::parse("page[@id='1']").unwrap_err();
        assert!(
            err.contains("XPath"),
            "should point at the XPath limitation: {err}"
        );
        assert!(Selector::parse("pa*ge").is_err());
    }

    #[test]
    fn any_match_checks_the_whole_selector_set() {
        let sels = vec![
            Selector::parse("page").unwrap(),
            Selector::parse("entry").unwrap(),
        ];
        assert!(any_match(&sels, &path(&["feed", "entry"])));
        assert!(any_match(&sels, &path(&["wiki", "page"])));
        assert!(!any_match(&sels, &path(&["wiki", "site"])));
        assert!(!any_match(&[], &path(&["page"])));
    }
}
