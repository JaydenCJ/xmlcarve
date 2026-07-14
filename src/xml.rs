//! A streaming pull parser over any `Read`, using constant memory.
//!
//! The parser keeps a single sliding byte buffer: bytes are refilled in
//! chunks, consumed token by token, and compacted away — so a 40 GB dump
//! needs the same few hundred kilobytes as a 4 KB sample. Well-formedness is
//! tracked with an open-element stack (memory proportional to nesting depth,
//! not file size).
//!
//! Deliberate liberties, chosen for real-world dump rescue:
//!
//! - Multiple root elements are allowed. Log-style XML streams that
//!   concatenate fragments without a wrapper are common and worth saving.
//! - `DOCTYPE` declarations (including an internal subset) are skipped, not
//!   resolved.
//! - Only UTF-8 input is accepted; a UTF-16 BOM produces a clear error
//!   instead of garbage.

use std::fmt;
use std::io::Read;

use crate::entity;

/// Refill granularity. Big enough to amortize syscalls, small enough that
/// "constant memory" stays an honest claim.
const CHUNK: usize = 64 * 1024;

/// One pull event. Names and attribute values arrive fully decoded.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The `<?xml ...?>` declaration, if present.
    Declaration,
    /// A `<!DOCTYPE ...>` declaration (contents skipped, not resolved).
    Doctype,
    /// A comment body (without `<!--` / `-->`).
    Comment(String),
    /// A processing instruction body (without `<?` / `?>`).
    Pi(String),
    /// An element start tag. Self-closing tags produce `Start` then `End`.
    Start {
        name: String,
        attributes: Vec<(String, String)>,
    },
    /// An element end tag, always paired with the matching `Start`.
    End { name: String },
    /// Character data with entities decoded.
    Text(String),
    /// A CDATA section, verbatim.
    CData(String),
    /// End of input; all open elements are closed.
    Eof,
}

/// A parse failure with the 1-based line where it was detected.
#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub message: String,
    pub line: u64,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for ParseError {}

/// Parser configuration.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParserOptions {
    /// Pass unknown named entities (`&nbsp;`) through verbatim instead of
    /// failing. Malformed references are still errors.
    pub lenient_entities: bool,
}

/// The streaming pull parser.
pub struct Parser<R: Read> {
    reader: R,
    buf: Vec<u8>,
    pos: usize,
    reader_done: bool,
    line: u64,
    bytes_read: u64,
    stack: Vec<String>,
    pending_end: Option<String>,
    seen_content: bool,
    seen_element: bool,
    finished: bool,
    opts: ParserOptions,
}

impl<R: Read> Parser<R> {
    pub fn new(reader: R, opts: ParserOptions) -> Self {
        Parser {
            reader,
            buf: Vec::with_capacity(CHUNK),
            pos: 0,
            reader_done: false,
            line: 1,
            bytes_read: 0,
            stack: Vec::new(),
            pending_end: None,
            seen_content: false,
            seen_element: false,
            finished: false,
            opts,
        }
    }

    /// Total bytes consumed from the underlying reader so far.
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read + self.pos as u64
    }

    /// 1-based line of the parser's current position.
    pub fn line(&self) -> u64 {
        self.line
    }

    /// Nesting depth of currently open elements.
    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    fn err<T>(&self, message: impl Into<String>) -> Result<T, ParseError> {
        Err(ParseError {
            message: message.into(),
            line: self.line,
        })
    }

    /// Pull one chunk from the reader into the buffer. Returns false at EOF.
    fn fill(&mut self) -> Result<bool, ParseError> {
        if self.reader_done {
            return Ok(false);
        }
        let old = self.buf.len();
        self.buf.resize(old + CHUNK, 0);
        match self.reader.read(&mut self.buf[old..]) {
            Ok(0) => {
                self.buf.truncate(old);
                self.reader_done = true;
                Ok(false)
            }
            Ok(n) => {
                self.buf.truncate(old + n);
                Ok(true)
            }
            Err(e) => {
                self.buf.truncate(old);
                Err(ParseError {
                    message: format!("read error: {e}"),
                    line: self.line,
                })
            }
        }
    }

    /// Drop already-consumed bytes so the buffer never grows past the size of
    /// one token plus one chunk. This is what makes memory constant.
    fn compact(&mut self) {
        if self.pos > 0 {
            self.bytes_read += self.pos as u64;
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
    }

    /// Byte at `pos + i`, refilling as needed.
    fn peek(&mut self, i: usize) -> Result<Option<u8>, ParseError> {
        while self.pos + i >= self.buf.len() {
            if !self.fill()? {
                return Ok(None);
            }
        }
        Ok(Some(self.buf[self.pos + i]))
    }

    /// True if the bytes at `pos + offset` start with `prefix` (refills).
    fn starts_with_at(&mut self, offset: usize, prefix: &[u8]) -> Result<bool, ParseError> {
        while self.pos + offset + prefix.len() > self.buf.len() {
            if !self.fill()? {
                return Ok(false);
            }
        }
        Ok(&self.buf[self.pos + offset..self.pos + offset + prefix.len()] == prefix)
    }

    /// Find `needle` starting at `pos + start`; returns the offset relative to
    /// `pos`. Scans incrementally so a giant text node is still one pass.
    fn find(&mut self, start: usize, needle: &[u8]) -> Result<Option<usize>, ParseError> {
        let mut from = self.pos + start;
        loop {
            let end = self.buf.len();
            if from + needle.len() <= end {
                for i in from..=(end - needle.len()) {
                    if &self.buf[i..i + needle.len()] == needle {
                        return Ok(Some(i - self.pos));
                    }
                }
                // Nothing found; a partial match may straddle the chunk edge.
                from = end - needle.len() + 1;
            }
            if !self.fill()? {
                return Ok(None);
            }
        }
    }

    /// Consume `n` bytes, updating the line counter.
    fn consume(&mut self, n: usize) {
        for &b in &self.buf[self.pos..self.pos + n] {
            if b == b'\n' {
                self.line += 1;
            }
        }
        self.pos += n;
    }

    /// Take `n` bytes as UTF-8, consuming them.
    fn take_str(&mut self, n: usize) -> Result<String, ParseError> {
        let s = match std::str::from_utf8(&self.buf[self.pos..self.pos + n]) {
            Ok(s) => s.to_string(),
            Err(e) => {
                return self.err(format!(
                    "invalid UTF-8 in input ({e}); only UTF-8 documents are supported"
                ))
            }
        };
        self.consume(n);
        Ok(s)
    }

    /// Pull the next event. After `Eof` it keeps returning `Eof`.
    pub fn next_event(&mut self) -> Result<Event, ParseError> {
        if let Some(name) = self.pending_end.take() {
            self.stack.pop();
            return Ok(Event::End { name });
        }
        if self.finished {
            return Ok(Event::Eof);
        }
        self.compact();
        if !self.seen_content {
            self.check_bom()?;
        }
        let Some(b) = self.peek(0)? else {
            if let Some(open) = self.stack.last() {
                return self.err(format!(
                    "unexpected end of file: {} unclosed element(s), <{}> still open",
                    self.stack.len(),
                    open
                ));
            }
            if !self.seen_element {
                return self.err("no XML element found in input");
            }
            self.finished = true;
            return Ok(Event::Eof);
        };
        self.seen_content = true;
        if b != b'<' {
            return self.parse_text();
        }
        match self.peek(1)? {
            None => self.err("unexpected end of file after '<'"),
            Some(b'!') => {
                if self.starts_with_at(2, b"--")? {
                    self.parse_comment()
                } else if self.starts_with_at(2, b"[CDATA[")? {
                    self.parse_cdata()
                } else if self.starts_with_at(2, b"DOCTYPE")?
                    || self.starts_with_at(2, b"doctype")?
                {
                    self.parse_doctype()
                } else {
                    self.err("unrecognized markup after '<!'")
                }
            }
            Some(b'?') => self.parse_pi(),
            Some(b'/') => self.parse_end_tag(),
            Some(_) => self.parse_start_tag(),
        }
    }

    fn check_bom(&mut self) -> Result<(), ParseError> {
        if self.starts_with_at(0, &[0xEF, 0xBB, 0xBF])? {
            self.consume(3); // UTF-8 BOM: harmless, skip it.
        } else if self.starts_with_at(0, &[0xFF, 0xFE])? || self.starts_with_at(0, &[0xFE, 0xFF])? {
            return self.err(
                "input looks like UTF-16 (byte-order mark found); re-encode to UTF-8 first, \
                 e.g. `iconv -f UTF-16 -t UTF-8`",
            );
        }
        Ok(())
    }

    fn parse_text(&mut self) -> Result<Event, ParseError> {
        let end = match self.find(0, b"<")? {
            Some(off) => off,
            None => self.buf.len() - self.pos, // trailing text up to EOF
        };
        let raw = self.take_str(end)?;
        match entity::decode(&raw, self.opts.lenient_entities) {
            Ok(s) => Ok(Event::Text(s)),
            Err(e) => self.err(e),
        }
    }

    fn parse_comment(&mut self) -> Result<Event, ParseError> {
        self.consume(4); // <!--
        let Some(end) = self.find(0, b"-->")? else {
            return self.err("unterminated comment (missing '-->')");
        };
        let body = self.take_str(end)?;
        self.consume(3);
        Ok(Event::Comment(body))
    }

    fn parse_cdata(&mut self) -> Result<Event, ParseError> {
        self.consume(9); // <![CDATA[
        let Some(end) = self.find(0, b"]]>")? else {
            return self.err("unterminated CDATA section (missing ']]>')");
        };
        let body = self.take_str(end)?;
        self.consume(3);
        Ok(Event::CData(body))
    }

    fn parse_doctype(&mut self) -> Result<Event, ParseError> {
        // Skip to the matching '>' while honoring quotes and one level of
        // internal subset brackets: <!DOCTYPE x [ <!ENTITY ...> ]>
        self.consume(2); // <!
        let mut i = 0;
        let mut depth = 0i32;
        let mut quote: Option<u8> = None;
        loop {
            let Some(b) = self.peek(i)? else {
                return self.err("unterminated DOCTYPE declaration");
            };
            match quote {
                Some(q) => {
                    if b == q {
                        quote = None;
                    }
                }
                None => match b {
                    b'"' | b'\'' => quote = Some(b),
                    b'[' | b'<' => depth += 1,
                    b']' => depth -= 1,
                    b'>' if depth <= 0 => {
                        self.consume(i + 1);
                        return Ok(Event::Doctype);
                    }
                    b'>' => depth -= 1,
                    _ => {}
                },
            }
            i += 1;
        }
    }

    fn parse_pi(&mut self) -> Result<Event, ParseError> {
        self.consume(2); // <?
        let Some(end) = self.find(0, b"?>")? else {
            return self.err("unterminated processing instruction (missing '?>')");
        };
        let body = self.take_str(end)?;
        self.consume(2);
        let target = body.split_whitespace().next().unwrap_or("");
        if target.eq_ignore_ascii_case("xml") {
            self.check_declared_encoding(&body)?;
            return Ok(Event::Declaration);
        }
        Ok(Event::Pi(body))
    }

    fn check_declared_encoding(&self, decl: &str) -> Result<(), ParseError> {
        let lower = decl.to_ascii_lowercase();
        let Some(idx) = lower.find("encoding") else {
            return Ok(());
        };
        let tail = &lower[idx + "encoding".len()..];
        let Some(q) = tail.find(['"', '\'']) else {
            return Ok(());
        };
        let quote = tail.as_bytes()[q] as char;
        let value = &tail[q + 1..];
        let Some(endq) = value.find(quote) else {
            return Ok(());
        };
        let enc = value[..endq].trim();
        if !matches!(enc, "utf-8" | "utf8" | "us-ascii" | "ascii") {
            return Err(ParseError {
                message: format!(
                    "declared encoding '{enc}' is not supported; re-encode the document to UTF-8"
                ),
                line: self.line,
            });
        }
        Ok(())
    }

    fn parse_end_tag(&mut self) -> Result<Event, ParseError> {
        self.consume(2); // </
        let Some(end) = self.find(0, b">")? else {
            return self.err("unterminated closing tag (missing '>')");
        };
        let raw = self.take_str(end)?;
        self.consume(1);
        let name = raw.trim();
        if name.is_empty() {
            return self.err("closing tag with empty name");
        }
        match self.stack.last() {
            None => self.err(format!("closing tag </{name}> with no element open")),
            Some(top) if top != name => self.err(format!(
                "mismatched closing tag: expected </{top}>, found </{name}>"
            )),
            Some(_) => {
                let name = self.stack.pop().unwrap();
                Ok(Event::End { name })
            }
        }
    }

    fn parse_start_tag(&mut self) -> Result<Event, ParseError> {
        // Find the closing '>' while honoring quoted attribute values, which
        // may legally contain '>' and newlines.
        let mut i = 1;
        let mut quote: Option<u8> = None;
        let end = loop {
            let Some(b) = self.peek(i)? else {
                return self.err("unterminated start tag (missing '>')");
            };
            match quote {
                Some(q) => {
                    if b == q {
                        quote = None;
                    }
                }
                None => match b {
                    b'"' | b'\'' => quote = Some(b),
                    b'>' => break i,
                    _ => {}
                },
            }
            i += 1;
        };
        self.consume(1); // <
        let raw = self.take_str(end - 1)?;
        self.consume(1); // >
        let (body, self_closing) = match raw.strip_suffix('/') {
            Some(b) => (b, true),
            None => (raw.as_str(), false),
        };
        let (name, attrs) = self.parse_tag_body(body)?;
        self.seen_element = true;
        self.stack.push(name.clone());
        if self_closing {
            self.pending_end = Some(name.clone());
        }
        Ok(Event::Start {
            name,
            attributes: attrs,
        })
    }

    /// Split `name attr="v" attr2='v'` into a name and decoded attributes.
    fn parse_tag_body(&self, body: &str) -> Result<(String, Vec<(String, String)>), ParseError> {
        let e = |msg: String| ParseError {
            message: msg,
            line: self.line,
        };
        let body = body.trim();
        let name_end = body.find(char::is_whitespace).unwrap_or(body.len());
        let name = &body[..name_end];
        if name.is_empty() {
            return Err(e("start tag with empty name".to_string()));
        }
        if name.contains(['=', '"', '\'', '<', '&']) {
            return Err(e(format!("invalid element name '{name}'")));
        }
        let mut attrs: Vec<(String, String)> = Vec::new();
        let mut rest = body[name_end..].trim_start();
        while !rest.is_empty() {
            let eq = rest
                .find('=')
                .ok_or_else(|| e(format!("attribute without value in <{name}>: '{rest}'")))?;
            let key = rest[..eq].trim();
            if key.is_empty() || key.contains(char::is_whitespace) || key.contains(['"', '\'']) {
                return Err(e(format!("invalid attribute name '{key}' in <{name}>")));
            }
            let after = rest[eq + 1..].trim_start();
            let mut chars = after.chars();
            let quote = match chars.next() {
                Some(q @ ('"' | '\'')) => q,
                _ => {
                    return Err(e(format!(
                        "attribute '{key}' in <{name}> is missing a quoted value"
                    )))
                }
            };
            let value_start = after.len() - chars.as_str().len();
            let Some(close) = after[value_start..].find(quote) else {
                return Err(e(format!(
                    "unterminated value for attribute '{key}' in <{name}>"
                )));
            };
            let raw_value = &after[value_start..value_start + close];
            if raw_value.contains('<') {
                return Err(e(format!(
                    "raw '<' in value of attribute '{key}' in <{name}>"
                )));
            }
            let value = entity::decode(raw_value, self.opts.lenient_entities).map_err(e)?;
            if attrs.iter().any(|(k, _)| k == key) {
                return Err(e(format!("duplicate attribute '{key}' in <{name}>")));
            }
            attrs.push((key.to_string(), value));
            rest = after[value_start + close + 1..].trim_start();
        }
        Ok((name.to_string(), attrs))
    }
}

#[cfg(test)]
mod tests {
    //! Parser tests feed byte slices through the same code path a
    //! multi-gigabyte file uses (the sliding buffer), so chunk-boundary
    //! behavior is exercised with a deliberately tiny reader below.

    use super::*;

    fn events(input: &str) -> Vec<Event> {
        events_with(input, ParserOptions::default())
    }

    fn events_with(input: &str, opts: ParserOptions) -> Vec<Event> {
        let mut p = Parser::new(input.as_bytes(), opts);
        let mut out = Vec::new();
        loop {
            let ev = p.next_event().expect("parse should succeed");
            let done = ev == Event::Eof;
            out.push(ev);
            if done {
                break;
            }
        }
        out
    }

    fn parse_err(input: &str) -> ParseError {
        let mut p = Parser::new(input.as_bytes(), ParserOptions::default());
        loop {
            match p.next_event() {
                Ok(Event::Eof) => panic!("expected a parse error for {input:?}"),
                Ok(_) => continue,
                Err(e) => return e,
            }
        }
    }

    fn start(name: &str, attrs: &[(&str, &str)]) -> Event {
        Event::Start {
            name: name.to_string(),
            attributes: attrs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    fn end(name: &str) -> Event {
        Event::End {
            name: name.to_string(),
        }
    }

    #[test]
    fn minimal_document_produces_start_text_end_eof() {
        assert_eq!(
            events("<a>hi</a>"),
            vec![
                start("a", &[]),
                Event::Text("hi".into()),
                end("a"),
                Event::Eof
            ]
        );
        // Whitespace inside the closing tag is tolerated.
        assert_eq!(
            events("<a></a >"),
            vec![start("a", &[]), end("a"), Event::Eof]
        );
        // And bytes_read matches what --stats will report.
        let doc = "<a>hello</a>";
        let mut p = Parser::new(doc.as_bytes(), ParserOptions::default());
        while p.next_event().unwrap() != Event::Eof {}
        assert_eq!(p.bytes_read(), doc.len() as u64);
    }

    #[test]
    fn self_closing_tag_produces_paired_start_and_end() {
        assert_eq!(events("<a/>"), vec![start("a", &[]), end("a"), Event::Eof]);
        assert_eq!(
            events("<a b=\"1\" />"),
            vec![start("a", &[("b", "1")]), end("a"), Event::Eof]
        );
    }

    #[test]
    fn attributes_support_both_quote_styles_and_entity_decoding() {
        assert_eq!(
            events(r#"<a x="1 &amp; 2" y='he said "hi"'/>"#),
            vec![
                start("a", &[("x", "1 & 2"), ("y", "he said \"hi\"")]),
                end("a"),
                Event::Eof
            ]
        );
    }

    #[test]
    fn attribute_values_may_contain_gt_and_newlines() {
        // '>' inside a quoted value must not terminate the tag early.
        assert_eq!(
            events("<a expr=\"1 > 0\nand more\">x</a>"),
            vec![
                start("a", &[("expr", "1 > 0\nand more")]),
                Event::Text("x".into()),
                end("a"),
                Event::Eof
            ]
        );
    }

    #[test]
    fn declaration_doctype_comment_pi_and_cdata_are_distinct_events() {
        let doc =
            "<?xml version=\"1.0\"?><!DOCTYPE r><r><!-- note --><?php x?><![CDATA[a<b&c]]></r>";
        assert_eq!(
            events(doc),
            vec![
                Event::Declaration,
                Event::Doctype,
                start("r", &[]),
                Event::Comment(" note ".into()),
                Event::Pi("php x".into()),
                Event::CData("a<b&c".into()),
                end("r"),
                Event::Eof
            ]
        );
    }

    #[test]
    fn doctype_internal_subset_with_quotes_and_brackets_is_skipped() {
        let doc = "<!DOCTYPE r [ <!ENTITY x \"weird ]> chars\"> ]><r/>";
        assert_eq!(
            events(doc),
            vec![Event::Doctype, start("r", &[]), end("r"), Event::Eof]
        );
    }

    #[test]
    fn text_entities_are_decoded_and_cdata_is_verbatim() {
        assert_eq!(
            events("<r>&lt;raw&gt;<![CDATA[&lt;kept&gt;]]></r>"),
            vec![
                start("r", &[]),
                Event::Text("<raw>".into()),
                Event::CData("&lt;kept&gt;".into()),
                end("r"),
                Event::Eof
            ]
        );
    }

    #[test]
    fn multiple_root_elements_are_allowed_for_log_style_streams() {
        assert_eq!(
            events("<r a=\"1\"/>\n<r a=\"2\"/>"),
            vec![
                start("r", &[("a", "1")]),
                end("r"),
                Event::Text("\n".into()),
                start("r", &[("a", "2")]),
                end("r"),
                Event::Eof
            ]
        );
    }

    #[test]
    fn encoding_guards_catch_utf16_boms_and_non_utf8_declarations() {
        // A UTF-8 BOM is harmless and skipped.
        let mut with_bom = vec![0xEF, 0xBB, 0xBF];
        with_bom.extend_from_slice(b"<a/>");
        let mut p = Parser::new(with_bom.as_slice(), ParserOptions::default());
        assert_eq!(p.next_event().unwrap(), start("a", &[]));

        // A UTF-16 BOM would decode to garbage; fail with advice instead.
        let utf16 = [0xFF, 0xFE, b'<', 0, b'a', 0];
        let mut p = Parser::new(&utf16[..], ParserOptions::default());
        let err = p.next_event().unwrap_err();
        assert!(err.message.contains("UTF-16"), "got: {err}");

        // A declared legacy encoding is rejected up front...
        let err = parse_err("<?xml version=\"1.0\" encoding=\"Shift_JIS\"?><a/>");
        assert!(err.message.contains("shift_jis"), "got: {err}");
        // ...while utf-8 spelled in any case is fine.
        assert_eq!(
            events("<?xml version=\"1.0\" encoding=\"UTF-8\"?><a/>"),
            vec![Event::Declaration, start("a", &[]), end("a"), Event::Eof]
        );
    }

    #[test]
    fn mismatched_closing_tag_reports_both_names_and_the_line() {
        let err = parse_err("<a>\n<b>\n</a>");
        assert_eq!(err.line, 3);
        assert!(
            err.message.contains("</b>") && err.message.contains("</a>"),
            "got: {err}"
        );
        // Line numbers count newlines inside text runs too.
        let err = parse_err("<a>\nline2\nline3<a x=1/>");
        assert_eq!(err.line, 3);
    }

    #[test]
    fn structural_errors_name_the_offending_element() {
        let err = parse_err("<a><b>text");
        assert!(
            err.message.contains("<b>") && err.message.contains("unclosed"),
            "got: {err}"
        );
        assert!(parse_err("</a>").message.contains("no element open"));
        // Empty, whitespace-only and comment-only inputs have no records to
        // carve; failing loudly beats emitting zero lines and exit 0.
        assert!(parse_err("").message.contains("no XML element"));
        assert!(parse_err("   \n  ").message.contains("no XML element"));
        assert!(parse_err("<!-- only a comment -->")
            .message
            .contains("no XML element"));
    }

    #[test]
    fn duplicate_and_unquoted_attributes_are_rejected_with_context() {
        let err = parse_err("<a x=\"1\" x=\"2\"/>");
        assert!(
            err.message.contains("duplicate attribute 'x'"),
            "got: {err}"
        );
        let err = parse_err("<a x=1/>");
        assert!(
            err.message.contains("'x'") && err.message.contains("quoted"),
            "got: {err}"
        );
    }

    /// A reader that returns one byte per `read()` call: the cruelest
    /// possible chunking. Every token boundary lands on a refill edge.
    struct OneByte<'a>(&'a [u8]);

    impl Read for OneByte<'_> {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            if self.0.is_empty() || out.is_empty() {
                return Ok(0);
            }
            out[0] = self.0[0];
            self.0 = &self.0[1..];
            Ok(1)
        }
    }

    #[test]
    fn one_byte_reads_produce_identical_events_to_slice_reads() {
        let doc =
            "<?xml version=\"1.0\"?><r a=\"x&amp;y\"><!-- c --><k>v1</k><k>v2</k><![CDATA[ ]]></r>";
        let mut p = Parser::new(OneByte(doc.as_bytes()), ParserOptions::default());
        let mut got = Vec::new();
        loop {
            let ev = p.next_event().unwrap();
            let done = ev == Event::Eof;
            got.push(ev);
            if done {
                break;
            }
        }
        assert_eq!(got, events(doc));
    }

    #[test]
    fn lenient_entities_flow_through_text_and_attributes() {
        let doc = "<a t=\"&nbsp;\">x&copy;y</a>";
        assert!(Parser::new(doc.as_bytes(), ParserOptions::default())
            .next_event()
            .is_err());
        assert_eq!(
            events_with(
                doc,
                ParserOptions {
                    lenient_entities: true
                }
            ),
            vec![
                start("a", &[("t", "&nbsp;")]),
                Event::Text("x&copy;y".into()),
                end("a"),
                Event::Eof
            ]
        );
    }

    #[test]
    fn invalid_utf8_bytes_are_a_clear_error_not_a_panic() {
        let doc = [b'<', b'a', b'>', 0xC3, 0x28, b'<', b'/', b'a', b'>'];
        let mut p = Parser::new(&doc[..], ParserOptions::default());
        p.next_event().unwrap(); // <a>
        let err = p.next_event().unwrap_err();
        assert!(err.message.contains("UTF-8"), "got: {err}");
    }
}
