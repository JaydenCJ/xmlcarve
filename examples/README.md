# Examples

Two small files that stand in for the giant dumps xmlcarve is built for.
Run everything from this directory with a built `xmlcarve` on your `PATH`
(or use `cargo run --`).

## wiki.xml — a MediaWiki-shaped export

Find the record element first, then carve:

```bash
xmlcarve inspect wiki.xml            # suggests --record: mediawiki/page
xmlcarve carve -r page wiki.xml      # one JSON line per <page>
```

Project a window out of the middle and count with your usual JSONL tools:

```bash
xmlcarve carve -r page --skip 1 --limit 1 wiki.xml
xmlcarve carve -r page wiki.xml | wc -l     # 3
```

## feed.xml — an Atom feed with namespaces and attributes

```bash
xmlcarve carve -r entry feed.xml
```

Attributes arrive as `@href`, `@rel`, `@term`; the two `<category>` elements
of the first entry collapse into an array. Try the mapping knobs:

```bash
xmlcarve carve -r entry --attr-prefix '' feed.xml     # no @ prefix
xmlcarve carve -r entry --wrap feed.xml               # {"entry": ...}
```

## Piping from anything

`-` reads stdin, so decompression streams straight through — no 40 GB
intermediate file:

```bash
bzcat dump.xml.bz2 | xmlcarve carve -r page --stats - > pages.jsonl
```
