//! XML entity and character-reference decoding.
//!
//! Handles the five predefined entities plus decimal/hexadecimal character
//! references. Unknown named entities are an error by default — legacy dumps
//! that embed HTML entities (`&nbsp;` and friends) can opt into passing them
//! through verbatim with lenient mode.

/// Longest entity body we accept between `&` and `;`. Anything longer is a
/// stray ampersand, not a reference — bail out with a clear error instead of
/// scanning to the end of a multi-gigabyte text node.
const MAX_ENTITY_LEN: usize = 40;

/// Decode all entity and character references in `input`.
///
/// With `lenient` set, unknown *named* entities are passed through verbatim
/// (`&nbsp;` stays `&nbsp;`); malformed references (unterminated, bad code
/// point) are always an error.
pub fn decode(input: &str, lenient: bool) -> Result<String, String> {
    if !input.contains('&') {
        return Ok(input.to_string());
    }
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let semi = match rest[1..]
            .char_indices()
            .take(MAX_ENTITY_LEN + 1)
            .find(|(_, c)| *c == ';')
        {
            Some((i, _)) => i + 1,
            None => return Err("unterminated entity reference (missing ';')".to_string()),
        };
        let body = &rest[1..semi];
        match resolve(body) {
            Resolved::Char(c) => out.push(c),
            Resolved::Unknown => {
                if lenient {
                    out.push_str(&rest[..semi + 1]);
                } else {
                    return Err(format!(
                        "unknown entity reference '&{body};' (only XML's five predefined entities \
                         and character references are decoded; lenient mode passes unknown named \
                         entities through)"
                    ));
                }
            }
            Resolved::Invalid(msg) => return Err(msg),
        }
        rest = &rest[semi + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

enum Resolved {
    Char(char),
    Unknown,
    Invalid(String),
}

fn resolve(body: &str) -> Resolved {
    match body {
        "amp" => return Resolved::Char('&'),
        "lt" => return Resolved::Char('<'),
        "gt" => return Resolved::Char('>'),
        "quot" => return Resolved::Char('"'),
        "apos" => return Resolved::Char('\''),
        _ => {}
    }
    let Some(num) = body.strip_prefix('#') else {
        // A named entity must look like a name; otherwise it is a stray '&'.
        if body.is_empty()
            || !body
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.')
        {
            return Resolved::Invalid(format!("malformed entity reference '&{body};'"));
        }
        return Resolved::Unknown;
    };
    let code = if let Some(hex) = num.strip_prefix('x').or_else(|| num.strip_prefix('X')) {
        u32::from_str_radix(hex, 16)
    } else {
        num.parse::<u32>()
    };
    match code {
        Ok(n) => match char::from_u32(n) {
            // XML forbids most C0 controls even via character references.
            Some(c) if n >= 0x20 || c == '\t' || c == '\n' || c == '\r' => Resolved::Char(c),
            _ => Resolved::Invalid(format!(
                "character reference '&#{num};' is not a valid XML character"
            )),
        },
        Err(_) => Resolved::Invalid(format!("malformed character reference '&#{num};'")),
    }
}

#[cfg(test)]
mod tests {
    //! Entity decoding is the first thing a messy legacy dump breaks, so the
    //! failure cases here matter as much as the happy path.

    use super::decode;

    #[test]
    fn decodes_all_five_predefined_entities_and_leaves_plain_text_alone() {
        assert_eq!(decode("hello <world>", false).unwrap(), "hello <world>");
        assert_eq!(
            decode("&lt;a b=&quot;c&apos;d&quot;&gt; &amp; more", false).unwrap(),
            "<a b=\"c'd\"> & more"
        );
        // Adjacent and repeated references decode independently.
        assert_eq!(decode("&amp;&amp;&lt;&gt;", false).unwrap(), "&&<>");
    }

    #[test]
    fn decodes_decimal_and_hex_character_references() {
        assert_eq!(
            decode("&#65;&#x42;&#x1F600;", false).unwrap(),
            "AB\u{1F600}"
        );
        // Uppercase X radix marker is accepted too.
        assert_eq!(decode("&#X42;", false).unwrap(), "B");
    }

    #[test]
    fn control_and_surrogate_code_points_are_rejected() {
        // Tab, LF and CR are the only control characters XML allows.
        assert_eq!(decode("&#9;&#10;&#13;", false).unwrap(), "\t\n\r");
        assert!(decode("&#0;", false).is_err());
        assert!(decode("&#7;", false).is_err());
        // 0xD800 is a UTF-16 surrogate; char::from_u32 refuses it, and so must we.
        let err = decode("&#xD800;", false).unwrap_err();
        assert!(err.contains("not a valid XML character"), "got: {err}");
    }

    #[test]
    fn unknown_named_entity_is_an_error_by_default() {
        let err = decode("a&nbsp;b", false).unwrap_err();
        assert!(
            err.contains("&nbsp;"),
            "error should name the entity: {err}"
        );
    }

    #[test]
    fn lenient_mode_passes_unknown_named_entities_through_verbatim() {
        assert_eq!(
            decode("a&nbsp;b&eacute;c", true).unwrap(),
            "a&nbsp;b&eacute;c"
        );
        // Known entities still decode in lenient mode.
        assert_eq!(decode("&amp;&nbsp;", true).unwrap(), "&&nbsp;");
    }

    #[test]
    fn unterminated_reference_is_reported_without_scanning_the_whole_node() {
        let long_tail = format!("&oops{}", "x".repeat(10_000));
        let err = decode(&long_tail, true).unwrap_err();
        assert!(err.contains("unterminated"), "got: {err}");
    }

    #[test]
    fn stray_ampersand_is_malformed_even_in_lenient_mode() {
        // "& " can never be a reference; silently keeping it would hide data
        // corruption, so lenient mode still rejects it.
        assert!(decode("fish & chips;", true).is_err());
        assert!(decode("&;", true).is_err());
    }
}
