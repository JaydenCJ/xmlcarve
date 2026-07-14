//! A minimal, ordered JSON value tree and serializer.
//!
//! Objects preserve insertion order (document order of the XML), which keeps
//! output byte-deterministic and diff-friendly — a `BTreeMap` would silently
//! reorder fields, a `HashMap` would randomize them.

use std::fmt::Write as _;

/// A JSON value. Numbers are split into integer and float variants so that
/// `--infer-types` can keep 64-bit IDs exact.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

impl Value {
    /// Serialize compactly (no whitespace), suitable for one JSONL line.
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Int(n) => {
                let _ = write!(out, "{n}");
            }
            Value::Float(f) => {
                // Finiteness is guarded at construction (record::infer);
                // Rust's Display for f64 is the shortest round-trip form,
                // which is deterministic across runs and platforms.
                let _ = write!(out, "{f}");
            }
            Value::Str(s) => write_escaped(s, out),
            Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
            Value::Object(entries) => {
                out.push('{');
                for (i, (k, v)) in entries.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_escaped(k, out);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }
}

/// Escape and quote a string per RFC 8259. Control characters use `\u00XX`;
/// everything else (including non-ASCII) passes through as UTF-8.
fn write_escaped(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    //! The serializer's whole job is byte-deterministic, spec-valid output;
    //! these tests pin the exact bytes.

    use super::Value;

    #[test]
    fn scalars_serialize_to_their_json_literals() {
        assert_eq!(Value::Null.to_json(), "null");
        assert_eq!(Value::Bool(true).to_json(), "true");
        assert_eq!(Value::Bool(false).to_json(), "false");
        assert_eq!(Value::Int(-42).to_json(), "-42");
        assert_eq!(Value::Int(i64::MAX).to_json(), "9223372036854775807");
        assert_eq!(Value::Float(2.5).to_json(), "2.5");
    }

    #[test]
    fn strings_escape_quotes_backslashes_and_controls() {
        assert_eq!(Value::Str("a\"b\\c".into()).to_json(), r#""a\"b\\c""#);
        assert_eq!(
            Value::Str("\n\r\t\u{08}\u{0C}".into()).to_json(),
            r#""\n\r\t\b\f""#
        );
        assert_eq!(
            Value::Str("\u{01}\u{1F}".into()).to_json(),
            "\"\\u0001\\u001f\""
        );
    }

    #[test]
    fn non_ascii_text_passes_through_unescaped() {
        // JSONL consumers expect UTF-8, not \uXXXX soup; this keeps CJK
        // wiki dumps readable.
        assert_eq!(
            Value::Str("日本語 emoji \u{1F600}".into()).to_json(),
            "\"日本語 emoji \u{1F600}\""
        );
    }

    #[test]
    fn arrays_and_objects_serialize_compactly_in_order() {
        let v = Value::Object(vec![
            ("b".into(), Value::Int(1)),
            (
                "a".into(),
                Value::Array(vec![Value::Null, Value::Str("x".into())]),
            ),
        ]);
        // "b" stays before "a": insertion order, not sorted order.
        assert_eq!(v.to_json(), r#"{"b":1,"a":[null,"x"]}"#);
        // Empty containers get no padding either.
        assert_eq!(Value::Array(vec![]).to_json(), "[]");
        assert_eq!(Value::Object(vec![]).to_json(), "{}");
    }

    #[test]
    fn keys_are_escaped_like_values() {
        let v = Value::Object(vec![("weird\"key".into(), Value::Null)]);
        assert_eq!(v.to_json(), r#"{"weird\"key":null}"#);
    }

    #[test]
    fn float_output_round_trips_through_parse() {
        for f in [0.1, 1e30, -2.5e-3, 123456.789] {
            let s = Value::Float(f).to_json();
            assert_eq!(s.parse::<f64>().unwrap(), f, "round-trip failed for {s}");
        }
    }
}
