//! Maps one XML record subtree onto a JSON value.
//!
//! The mapping is the widely-understood xmltodict convention, made explicit
//! and deterministic (full rules in `docs/mapping.md`):
//!
//! - attributes become keys with a configurable prefix (default `@`);
//! - repeated child elements collapse into an array, in document order;
//! - a text-only element becomes a plain string, preserved verbatim;
//! - mixed content keeps its non-whitespace text under a configurable
//!   text key (default `#text`);
//! - an empty element becomes `null`.

use crate::json::Value;

/// Mapping configuration shared by `carve` and the CLI.
#[derive(Debug, Clone, PartialEq)]
pub struct MapOptions {
    /// Prefix for attribute keys (default `@`). May be empty.
    pub attr_prefix: String,
    /// Key for text in mixed content (default `#text`).
    pub text_key: String,
    /// Strip namespace prefixes from element/attribute names and drop
    /// `xmlns` declarations entirely.
    pub strip_namespaces: bool,
    /// Convert text and attribute values that look like numbers or booleans
    /// into JSON numbers and booleans.
    pub infer_types: bool,
    /// Wrap each record as `{"<element name>": ...}`.
    pub wrap: bool,
}

impl Default for MapOptions {
    fn default() -> Self {
        MapOptions {
            attr_prefix: "@".to_string(),
            text_key: "#text".to_string(),
            strip_namespaces: false,
            infer_types: false,
            wrap: false,
        }
    }
}

/// Strip a `prefix:` from an XML name, if namespace stripping is on.
pub fn map_name(name: &str, opts: &MapOptions) -> String {
    if opts.strip_namespaces {
        match name.rsplit_once(':') {
            Some((_, local)) if !local.is_empty() => local.to_string(),
            _ => name.to_string(),
        }
    } else {
        name.to_string()
    }
}

/// One text chunk; CDATA is tracked so explicit `<![CDATA[ ]]>` whitespace
/// survives mixed-content filtering.
#[derive(Debug)]
struct Chunk {
    text: String,
    cdata: bool,
}

#[derive(Debug)]
struct Frame {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<(String, Value)>,
    chunks: Vec<Chunk>,
}

/// Incrementally builds the JSON value for one record while the carve driver
/// streams events into it. Only the *current* record subtree is ever held in
/// memory — this is the constant-memory contract.
#[derive(Debug)]
pub struct RecordBuilder {
    frames: Vec<Frame>,
    opts: MapOptions,
}

impl RecordBuilder {
    pub fn new(opts: MapOptions) -> Self {
        RecordBuilder {
            frames: Vec::new(),
            opts,
        }
    }

    /// Open an element. Names arrive raw; mapping is applied here.
    pub fn start(&mut self, name: &str, attributes: &[(String, String)]) {
        let mut attrs = Vec::with_capacity(attributes.len());
        for (k, v) in attributes {
            if self.opts.strip_namespaces && (k == "xmlns" || k.starts_with("xmlns:")) {
                continue;
            }
            attrs.push((map_name(k, &self.opts), v.clone()));
        }
        self.frames.push(Frame {
            name: map_name(name, &self.opts),
            attrs,
            children: Vec::new(),
            chunks: Vec::new(),
        });
    }

    /// Append character data to the innermost open element.
    pub fn text(&mut self, text: &str, cdata: bool) {
        if let Some(frame) = self.frames.last_mut() {
            frame.chunks.push(Chunk {
                text: text.to_string(),
                cdata,
            });
        }
    }

    /// Close the innermost element. Returns the finished record value when
    /// the outermost element of the record closes, `None` otherwise.
    pub fn end(&mut self) -> Option<Value> {
        let frame = self.frames.pop().expect("end() without matching start()");
        let (name, value) = self.finalize(frame);
        match self.frames.last_mut() {
            Some(parent) => {
                parent.children.push((name, value));
                None
            }
            None => Some(if self.opts.wrap {
                Value::Object(vec![(name, value)])
            } else {
                value
            }),
        }
    }

    /// True while an element of the record is still open.
    pub fn in_progress(&self) -> bool {
        !self.frames.is_empty()
    }

    fn finalize(&self, frame: Frame) -> (String, Value) {
        let Frame {
            name,
            attrs,
            children,
            chunks,
        } = frame;
        let text_only = attrs.is_empty() && children.is_empty();

        if text_only {
            // Verbatim: whitespace in a text-only element is data.
            let joined: String = chunks.iter().map(|c| c.text.as_str()).collect();
            let value = if joined.is_empty() && chunks.iter().all(|c| !c.cdata) {
                Value::Null
            } else {
                self.scalar(joined)
            };
            return (name, value);
        }

        let mut entries: Vec<(String, Value)> = Vec::new();
        for (k, v) in attrs {
            entries.push((format!("{}{}", self.opts.attr_prefix, k), self.scalar(v)));
        }
        for (k, v) in children {
            merge_child(&mut entries, k, v);
        }
        // Mixed content: indentation whitespace between children is noise,
        // but explicit CDATA whitespace is intentional and kept.
        let kept: String = chunks
            .iter()
            .filter(|c| c.cdata || !c.text.chars().all(char::is_whitespace))
            .map(|c| c.text.as_str())
            .collect();
        if !kept.is_empty() {
            entries.push((self.opts.text_key.clone(), self.scalar(kept)));
        }
        (name, Value::Object(entries))
    }

    fn scalar(&self, text: String) -> Value {
        if self.opts.infer_types {
            infer(&text)
        } else {
            Value::Str(text)
        }
    }
}

/// Merge a child into the entry list, promoting repeats to an array.
fn merge_child(entries: &mut Vec<(String, Value)>, key: String, value: Value) {
    if let Some((_, existing)) = entries.iter_mut().find(|(k, _)| *k == key) {
        match existing {
            Value::Array(items) => items.push(value),
            _ => {
                let first = std::mem::replace(existing, Value::Null);
                *existing = Value::Array(vec![first, value]);
            }
        }
    } else {
        entries.push((key, value));
    }
}

/// Conservative type inference: only exact `true`/`false`, i64-range
/// integers without leading zeros, and finite decimal floats convert.
/// Anything ambiguous (`007`, `1e999`, `NaN`, `+5`, ``) stays a string so
/// no information is lost.
pub fn infer(text: &str) -> Value {
    match text {
        "true" => return Value::Bool(true),
        "false" => return Value::Bool(false),
        _ => {}
    }
    let digits = text.strip_prefix('-').unwrap_or(text);
    if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
        // "007" and "-0" round-trip badly as numbers; keep them as strings.
        if digits.len() > 1 && digits.starts_with('0') {
            return Value::Str(text.to_string());
        }
        if text == "-0" {
            return Value::Str(text.to_string());
        }
        if let Ok(n) = text.parse::<i64>() {
            return Value::Int(n);
        }
        return Value::Str(text.to_string()); // beyond i64: preserve exactly
    }
    if looks_like_float(text) {
        if let Ok(f) = text.parse::<f64>() {
            if f.is_finite() {
                return Value::Float(f);
            }
        }
    }
    Value::Str(text.to_string())
}

/// Strict float shape: `-?(\d+\.\d*|\.\d+|\d+)([eE][+-]?\d+)?` with at least
/// one of a dot or an exponent (plain integers are handled above).
fn looks_like_float(text: &str) -> bool {
    let s = text.strip_prefix('-').unwrap_or(text);
    let (mantissa, exponent) = match s.find(['e', 'E']) {
        Some(i) => (&s[..i], Some(&s[i + 1..])),
        None => (s, None),
    };
    let mantissa_ok = match mantissa.split_once('.') {
        Some((int, frac)) => {
            (!int.is_empty() || !frac.is_empty())
                && int.bytes().all(|b| b.is_ascii_digit())
                && frac.bytes().all(|b| b.is_ascii_digit())
        }
        None => !mantissa.is_empty() && mantissa.bytes().all(|b| b.is_ascii_digit()),
    };
    if !mantissa_ok {
        return false;
    }
    match exponent {
        None => mantissa.contains('.'),
        Some(e) => {
            let e = e.strip_prefix(['+', '-']).unwrap_or(e);
            !e.is_empty() && e.bytes().all(|b| b.is_ascii_digit())
        }
    }
}

#[cfg(test)]
mod tests {
    //! Builder tests drive the same start/text/end sequence the carve driver
    //! produces, then pin the serialized JSON — mapping rules are contracts.

    use super::*;

    /// Tiny event DSL: ("+name", attrs) opens, ("-",) closes, ("t", s) text.
    fn build(opts: MapOptions, script: &[(&str, &[(&str, &str)])]) -> Value {
        let mut b = RecordBuilder::new(opts);
        let mut result = None;
        for (op, attrs) in script {
            if let Some(name) = op.strip_prefix('+') {
                let attrs: Vec<(String, String)> = attrs
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect();
                b.start(name, &attrs);
            } else if *op == "-" {
                result = b.end();
            } else if let Some(t) = op.strip_prefix('t') {
                b.text(t, false);
            } else if let Some(t) = op.strip_prefix('c') {
                b.text(t, true);
            }
        }
        result.expect("script should close the record")
    }

    const NO_ATTRS: &[(&str, &str)] = &[];

    #[test]
    fn text_only_element_becomes_a_plain_string_preserved_verbatim() {
        let v = build(
            MapOptions::default(),
            &[("+title", NO_ATTRS), ("tHello", NO_ATTRS), ("-", NO_ATTRS)],
        );
        assert_eq!(v.to_json(), r#""Hello""#);
        // Wiki page bodies keep meaningful leading/trailing newlines.
        let v = build(
            MapOptions::default(),
            &[
                ("+t", NO_ATTRS),
                ("t\n  body \n", NO_ATTRS),
                ("-", NO_ATTRS),
            ],
        );
        assert_eq!(v.to_json(), "\"\\n  body \\n\"");
    }

    #[test]
    fn empty_element_becomes_null_but_empty_cdata_becomes_empty_string() {
        let empty = build(MapOptions::default(), &[("+a", NO_ATTRS), ("-", NO_ATTRS)]);
        assert_eq!(empty, Value::Null);
        // <a><![CDATA[]]></a> is an *explicit* empty string, not absence.
        let cdata = build(
            MapOptions::default(),
            &[("+a", NO_ATTRS), ("c", NO_ATTRS), ("-", NO_ATTRS)],
        );
        assert_eq!(cdata, Value::Str(String::new()));
    }

    #[test]
    fn attributes_get_the_at_prefix_and_come_first() {
        let v = build(
            MapOptions::default(),
            &[
                ("+page", &[("id", "7"), ("ns", "0")]),
                ("tX", NO_ATTRS),
                ("-", NO_ATTRS),
            ],
        );
        assert_eq!(v.to_json(), r##"{"@id":"7","@ns":"0","#text":"X"}"##);
    }

    #[test]
    fn attr_prefix_and_text_key_are_configurable() {
        let opts = MapOptions {
            attr_prefix: "_".to_string(),
            text_key: "value".to_string(),
            ..MapOptions::default()
        };
        let v = build(
            opts,
            &[("+a", &[("k", "v")]), ("thi", NO_ATTRS), ("-", NO_ATTRS)],
        );
        assert_eq!(v.to_json(), r#"{"_k":"v","value":"hi"}"#);
    }

    #[test]
    fn repeated_children_collapse_into_an_array_in_document_order() {
        let v = build(
            MapOptions::default(),
            &[
                ("+links", NO_ATTRS),
                ("+a", NO_ATTRS),
                ("t1", NO_ATTRS),
                ("-", NO_ATTRS),
                ("+b", NO_ATTRS),
                ("tmid", NO_ATTRS),
                ("-", NO_ATTRS),
                ("+a", NO_ATTRS),
                ("t2", NO_ATTRS),
                ("-", NO_ATTRS),
                ("+a", NO_ATTRS),
                ("t3", NO_ATTRS),
                ("-", NO_ATTRS),
                ("-", NO_ATTRS),
            ],
        );
        // "a" keeps its first position; "b" is not disturbed.
        assert_eq!(v.to_json(), r#"{"a":["1","2","3"],"b":"mid"}"#);
        // A single child stays scalar — never a one-element array.
        let v = build(
            MapOptions::default(),
            &[
                ("+r", NO_ATTRS),
                ("+k", NO_ATTRS),
                ("tv", NO_ATTRS),
                ("-", NO_ATTRS),
                ("-", NO_ATTRS),
            ],
        );
        assert_eq!(v.to_json(), r#"{"k":"v"}"#);
    }

    #[test]
    fn indentation_whitespace_between_children_is_dropped() {
        let v = build(
            MapOptions::default(),
            &[
                ("+r", NO_ATTRS),
                ("t\n  ", NO_ATTRS),
                ("+k", NO_ATTRS),
                ("tv", NO_ATTRS),
                ("-", NO_ATTRS),
                ("t\n", NO_ATTRS),
                ("-", NO_ATTRS),
            ],
        );
        assert_eq!(v.to_json(), r#"{"k":"v"}"#);
    }

    #[test]
    fn mixed_content_keeps_real_text_under_the_text_key() {
        let v = build(
            MapOptions::default(),
            &[
                ("+p", NO_ATTRS),
                ("tsee ", NO_ATTRS),
                ("+b", NO_ATTRS),
                ("there", NO_ATTRS),
                ("-", NO_ATTRS),
                ("t for details", NO_ATTRS),
                ("-", NO_ATTRS),
            ],
        );
        assert_eq!(v.to_json(), r##"{"b":"here","#text":"see  for details"}"##);
        // Whitespace-only CDATA is explicit and survives the filtering that
        // drops indentation whitespace.
        let v = build(
            MapOptions::default(),
            &[
                ("+r", NO_ATTRS),
                ("+k", NO_ATTRS),
                ("tv", NO_ATTRS),
                ("-", NO_ATTRS),
                ("c  ", NO_ATTRS), // explicit <![CDATA[  ]]>
                ("-", NO_ATTRS),
            ],
        );
        assert_eq!(v.to_json(), r##"{"k":"v","#text":"  "}"##);
    }

    #[test]
    fn wrap_option_nests_the_record_under_its_element_name() {
        let opts = MapOptions {
            wrap: true,
            ..MapOptions::default()
        };
        let v = build(
            opts,
            &[("+page", NO_ATTRS), ("thi", NO_ATTRS), ("-", NO_ATTRS)],
        );
        assert_eq!(v.to_json(), r#"{"page":"hi"}"#);
    }

    #[test]
    fn strip_namespaces_removes_prefixes_and_xmlns_declarations() {
        let opts = MapOptions {
            strip_namespaces: true,
            ..MapOptions::default()
        };
        let v = build(
            opts,
            &[
                (
                    "+dc:record",
                    &[("xmlns:dc", "http://example.test/dc"), ("dc:id", "9")],
                ),
                ("+dc:title", NO_ATTRS),
                ("tT", NO_ATTRS),
                ("-", NO_ATTRS),
                ("-", NO_ATTRS),
            ],
        );
        assert_eq!(v.to_json(), r#"{"@id":"9","title":"T"}"#);
    }

    #[test]
    fn infer_types_converts_text_and_attribute_scalars() {
        let opts = MapOptions {
            infer_types: true,
            ..MapOptions::default()
        };
        let v = build(
            opts,
            &[
                ("+r", &[("id", "42")]),
                ("+ok", NO_ATTRS),
                ("ttrue", NO_ATTRS),
                ("-", NO_ATTRS),
                ("+score", NO_ATTRS),
                ("t2.5", NO_ATTRS),
                ("-", NO_ATTRS),
                ("+name", NO_ATTRS),
                ("tbob", NO_ATTRS),
                ("-", NO_ATTRS),
                ("-", NO_ATTRS),
            ],
        );
        assert_eq!(
            v.to_json(),
            r#"{"@id":42,"ok":true,"score":2.5,"name":"bob"}"#
        );
    }

    #[test]
    fn infer_keeps_ambiguous_numbers_as_strings() {
        use super::infer;
        // Leading zeros are identifiers (postal codes, phone numbers).
        assert_eq!(infer("007"), Value::Str("007".into()));
        assert_eq!(infer("-0"), Value::Str("-0".into()));
        // Beyond i64: converting to f64 would silently lose precision.
        assert_eq!(
            infer("92233720368547758080"),
            Value::Str("92233720368547758080".into())
        );
        // Float shapes that Rust would happily parse but JSON cannot express.
        assert_eq!(infer("NaN"), Value::Str("NaN".into()));
        assert_eq!(infer("inf"), Value::Str("inf".into()));
        assert_eq!(infer("1e999"), Value::Str("1e999".into()));
        // Signed-plus and empty strings are not numbers either.
        assert_eq!(infer("+5"), Value::Str("+5".into()));
        assert_eq!(infer(""), Value::Str("".into()));
        assert_eq!(infer("1.2.3"), Value::Str("1.2.3".into()));
        assert_eq!(infer("True"), Value::Str("True".into()));
    }

    #[test]
    fn infer_accepts_the_full_strict_float_grammar() {
        use super::infer;
        assert_eq!(infer("0.5"), Value::Float(0.5));
        assert_eq!(infer("-0.5"), Value::Float(-0.5));
        assert_eq!(infer(".5"), Value::Float(0.5));
        assert_eq!(infer("5."), Value::Float(5.0));
        assert_eq!(infer("1e3"), Value::Float(1000.0));
        assert_eq!(infer("1.5E-2"), Value::Float(0.015));
        assert_eq!(infer("2e+1"), Value::Float(20.0));
        assert_eq!(infer("-9007199254775807"), Value::Int(-9007199254775807));
        assert_eq!(infer("0"), Value::Int(0));
    }

    #[test]
    fn deeply_nested_structure_maps_recursively() {
        let v = build(
            MapOptions::default(),
            &[
                ("+page", NO_ATTRS),
                ("+revision", NO_ATTRS),
                ("+contributor", NO_ATTRS),
                ("+username", NO_ATTRS),
                ("tAda", NO_ATTRS),
                ("-", NO_ATTRS),
                ("-", NO_ATTRS),
                ("-", NO_ATTRS),
                ("-", NO_ATTRS),
            ],
        );
        assert_eq!(
            v.to_json(),
            r#"{"revision":{"contributor":{"username":"Ada"}}}"#
        );
        // in_progress() reports whether any frame of the record is still open.
        let mut b = RecordBuilder::new(MapOptions::default());
        assert!(!b.in_progress());
        b.start("a", &[]);
        b.start("b", &[]);
        assert!(b.in_progress());
        assert_eq!(b.end(), None); // closes <b>, record still open
        assert!(b.in_progress());
        assert!(b.end().is_some());
        assert!(!b.in_progress());
    }
}
