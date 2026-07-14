# Contributing to xmlcarve

Thanks for your interest in improving xmlcarve. Issues, discussions and pull requests are all welcome.

## Getting started

Prerequisites: Rust 1.75 or newer (stable toolchain).

```bash
git clone https://github.com/JaydenCJ/xmlcarve.git
cd xmlcarve
cargo build
cargo test
bash scripts/smoke.sh
```

`scripts/smoke.sh` runs the real binary end to end — inspect-then-carve on the bundled examples, stdin streaming, windowing flags, and the error paths. It finishes in well under a minute and must print `SMOKE OK`.

## Before you open a pull request

1. `cargo fmt` — formatting is enforced.
2. `cargo clippy --all-targets -- -D warnings` — clippy must be clean.
3. `cargo test` — unit tests and the CLI integration tests must pass.
4. `bash scripts/smoke.sh` — the smoke test must print `SMOKE OK`.
5. Add tests for behavior changes. Parsing, mapping and selection live in pure modules (`xml`, `entity`, `record`, `selector`, `json`) that are easy to unit-test; please keep it that way.

## Ground rules

- Keep dependencies at zero. The pull parser, entity decoder, JSON writer and selector engine are deliberately std-only; adding a dependency needs a very strong justification in the PR description.
- Constant memory is the contract: nothing outside the current record subtree may be buffered, no matter what feature you add. If a change can grow memory with input size, it needs a different design.
- No network calls, no telemetry, ever. xmlcarve reads a file or stdin and writes JSONL — that is the whole surface.
- Output is deterministic: same input + same flags = byte-identical JSONL. Object key order, float formatting and string escaping are all pinned by tests.
- Code comments and doc comments are written in English.

## Reporting bugs

Please include the `xmlcarve --version` output, the exact command line, and a minimal XML sample that reproduces the issue (redact content if needed — structure is what matters). For parse errors, the reported line number and the surrounding few lines of input make fixes fast.

## Security

If you find a security issue (e.g. memory exhaustion via crafted input), please do not open a public issue. Use GitHub's private vulnerability reporting on this repository instead.
