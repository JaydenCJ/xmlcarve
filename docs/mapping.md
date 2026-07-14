# XML → JSON mapping rules

xmlcarve converts each record subtree with a small, deterministic rule set —
the widely-understood xmltodict convention, pinned down precisely. Every rule
below is enforced by a unit test in `src/record.rs`.

## The rules

1. **Attributes** become object keys prefixed with `@` (`--attr-prefix`
   changes or removes the prefix). Attribute keys always come before child
   keys, in document order.

   ```xml
   <page id="7" ns="0">X</page>  →  {"@id":"7","@ns":"0","#text":"X"}
   ```

2. **Child elements** become keys in document order. When the same child name
   repeats, the values collapse into an array at the position of the first
   occurrence. A child that appears once stays scalar — consumers that need a
   stable shape should normalize downstream (`jq '[.link] | flatten'`) or use
   schema-aware tooling.

   ```xml
   <r><a>1</a><b>mid</b><a>2</a></r>  →  {"a":["1","2"],"b":"mid"}
   ```

3. **Text-only elements** (no attributes, no children) become plain JSON
   strings, preserved verbatim — leading/trailing whitespace and newlines
   included, because wiki page bodies and code samples are data.

4. **Empty elements** (`<a/>`, `<a></a>`) become `null`. An explicit empty
   CDATA section (`<a><![CDATA[]]></a>`) becomes `""` — presence was stated.

5. **Mixed content** (text alongside attributes or children) keeps its text
   under `#text` (`--text-key` renames it). Whitespace-only text chunks —
   pretty-printing indentation — are dropped; whitespace-only **CDATA**
   chunks are kept, because CDATA is always intentional. Remaining chunks are
   concatenated in document order; element positions within the text are not
   preserved (mixed-content markup is inherently lossy in JSON).

6. **CDATA** is text with no entity decoding: `<![CDATA[a & <b>]]>` yields
   the literal string `a & <b>`.

7. **Namespaces** are kept verbatim by default: element `dc:title` maps to
   key `"dc:title"`. With `--strip-namespaces`, prefixes are removed from
   element and attribute names, and `xmlns`/`xmlns:*` declarations are
   dropped entirely. Selector matching sees the same names the mapping does,
   so with stripping on you write `--record page`, not `--record w:page`.

8. **Type inference** is off by default (everything is a string, lossless).
   With `--infer-types`, text and attribute values convert conservatively:
   - `true` / `false` → booleans (exact match only; `True` stays a string);
   - integers within i64 → numbers, **except** leading-zero forms (`007`)
     and `-0`, which stay strings (postal codes, phone numbers);
   - decimal/exponent floats (`2.5`, `1e3`, `-1.5E-2`) → numbers, but only
     when finite (`1e999`, `NaN`, `inf` stay strings);
   - anything else — including out-of-i64-range integers — stays a string
     so no precision is silently lost.

## Determinism

The same input and flags always produce byte-identical output: object keys
keep insertion order, floats use Rust's shortest round-trip formatting, and
strings escape per RFC 8259 (control characters as `\u00XX`, everything else
as raw UTF-8).

## Record framing

One record = one matched element subtree = one line of JSON, terminated by
`\n`. Records never nest: while a record is being built, further selector
matches inside it are ordinary child elements (a `<page>` inside a `<page>`
does not restart the frame).
