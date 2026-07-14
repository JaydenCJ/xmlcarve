# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-07-13

### Added

- Streaming pull parser over any reader with a constant-memory sliding buffer: elements, attributes, text, CDATA, comments, processing instructions, DOCTYPE skipping (including internal subsets), UTF-8 BOM handling, and clear errors (with line numbers) for UTF-16 input, declared legacy encodings, and malformed markup.
- Entity decoding: the five predefined XML entities plus decimal/hex character references, with XML character-range validation; `--lenient` passes unknown named entities (`&nbsp;`) through verbatim for messy legacy dumps.
- Record selectors: `page` (any depth), `feed/entry` (parent-path suffix), `/root/items/item` (anchored at the root), `*` wildcards; repeatable `--record` carves a union.
- Deterministic XML→JSON mapping (documented in `docs/mapping.md`): `@`-prefixed attributes, repeated children as arrays, verbatim text-only strings, `#text` for mixed content, `null` for empty elements — with `--attr-prefix`, `--text-key`, `--wrap`, `--strip-namespaces`, and conservative `--infer-types` knobs.
- `xmlcarve carve`: file or stdin input, stdout or `--output` file, `--skip`/`--limit` record windows (`--limit` stops reading the input early), `--stats` summary on stderr, partial-rescue semantics (records before a corruption point are already written), and broken-pipe-friendly exit codes.
- `xmlcarve inspect`: one-pass structure profiler that counts every distinct element path, flags attribute-bearing elements, and suggests a `--record` selector; `--limit` profiles a giant file from its first slice.
- Multiple root elements accepted deliberately, so log-style concatenated XML fragment streams carve without a wrapper document.
- Test suite: 80 unit tests, 10 CLI integration tests, and `scripts/smoke.sh`.

[0.1.0]: https://github.com/JaydenCJ/xmlcarve/releases/tag/v0.1.0
