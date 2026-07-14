#!/usr/bin/env bash
# Smoke test: builds xmlcarve, then exercises the real CLI end to end —
# inspect-then-carve on a wiki-shaped dump, stdin streaming, skip/limit
# windows, mapping flags, and the error paths (exit codes, line numbers).
# Self-contained: temp dirs only, no network.
set -euo pipefail

cd "$(dirname "$0")/.."

fail() { echo "SMOKE FAIL: $*" >&2; exit 1; }

echo "[smoke] building..."
cargo build --quiet
BIN=target/debug/xmlcarve

WORK=$(mktemp -d "${TMPDIR:-/tmp}/xmlcarve-smoke.XXXXXX")
trap 'rm -rf "$WORK"' EXIT

# --- 1. version/help sanity -------------------------------------------------
"$BIN" --version | grep -q '^xmlcarve 0\.1\.0$' || fail "--version mismatch"
"$BIN" --help | grep -q 'COMMANDS:' || fail "--help missing sections"

# --- 2. inspect suggests the record element ---------------------------------
echo "[smoke] xmlcarve inspect"
"$BIN" inspect examples/wiki.xml | tee "$WORK/inspect.out"
grep -q 'suggested --record: mediawiki/page' "$WORK/inspect.out" \
  || fail "inspect did not suggest mediawiki/page"
grep -qE '^\s+3\s+.*mediawiki/page$' "$WORK/inspect.out" \
  || fail "inspect did not count 3 pages"

# --- 3. carve the suggested selector ----------------------------------------
echo "[smoke] xmlcarve carve -r page"
"$BIN" carve -r page --stats examples/wiki.xml > "$WORK/pages.jsonl" 2> "$WORK/stats.err"
[ "$(wc -l < "$WORK/pages.jsonl")" = 3 ] || fail "expected 3 JSONL lines"
grep -q '"title":"Streaming parser"' "$WORK/pages.jsonl" || fail "first page missing"
grep -q '"title":"Pull & push APIs"' "$WORK/pages.jsonl" || fail "entities not decoded"
grep -q '"@bytes":"53"' "$WORK/pages.jsonl" || fail "attributes not mapped"
grep -q 'wrote 3 record(s) (3 matched, 0 skipped)' "$WORK/stats.err" || fail "--stats summary wrong"
grep -q 'Testwiki' "$WORK/pages.jsonl" && fail "siteinfo leaked into records"

# Every line must be valid JSON (verified with python3 if available).
if command -v python3 >/dev/null 2>&1; then
  python3 -c 'import json,sys; [json.loads(l) for l in sys.stdin]' < "$WORK/pages.jsonl" \
    || fail "output is not valid JSONL"
  echo "[smoke] all lines parse as JSON"
fi

# --- 4. stdin streaming + skip/limit window + type inference -----------------
echo "[smoke] stdin window with --infer-types"
cat examples/wiki.xml \
  | "$BIN" carve -r page --skip 1 --limit 1 --infer-types --wrap - > "$WORK/window.jsonl"
[ "$(wc -l < "$WORK/window.jsonl")" = 1 ] || fail "window should hold exactly 1 record"
grep -q '^{"page":' "$WORK/window.jsonl" || fail "--wrap missing"
grep -q '"id":2' "$WORK/window.jsonl" || fail "--infer-types did not produce a number"

# --- 5. namespaces + repeated children on the Atom feed ----------------------
echo "[smoke] atom feed with --strip-namespaces"
"$BIN" carve -r entry --strip-namespaces -o "$WORK/entries.jsonl" examples/feed.xml
[ "$(wc -l < "$WORK/entries.jsonl")" = 2 ] || fail "expected 2 entries"
grep -q '"category":\[' "$WORK/entries.jsonl" || fail "repeated children did not become an array"

# --- 6. error paths keep their contracts -------------------------------------
echo "[smoke] error paths"
printf '<r>\n<p><id>1</id></p>\n<p></oops>\n</r>\n' > "$WORK/broken.xml"
set +e
"$BIN" carve -r p "$WORK/broken.xml" > "$WORK/partial.jsonl" 2> "$WORK/broken.err"
CODE=$?
set -e
[ "$CODE" = 1 ] || fail "malformed XML should exit 1, got $CODE"
grep -q 'line 3' "$WORK/broken.err" || fail "parse error lacks line number"
[ "$(wc -l < "$WORK/partial.jsonl")" = 1 ] || fail "good record before damage not rescued"

set +e
"$BIN" carve "$WORK/broken.xml" 2> "$WORK/usage.err"
CODE=$?
set -e
[ "$CODE" = 2 ] || fail "missing --record should exit 2, got $CODE"
grep -q 'inspect' "$WORK/usage.err" || fail "usage error should point at inspect"

echo "SMOKE OK"
