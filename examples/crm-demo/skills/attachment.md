---
type: skill
id: attachment
description: An uploaded document (text/markdown) ingested through the document backend — extracted, chunked, and embedded into one page-with-blocks. Read-only; the original blob is canonical.
backend:
  kind: document
  accepts: [text/plain, text/markdown]
  chunk:
    max_chars: 800
    overlap: 80
optional_frontmatter: [title, source, about]
---

# attachment

A **document-backed** skill: its instances aren't authored as markdown
but *uploaded*. An external client deposits a blob and POSTs `/ingest`
(or `/ingest/upload`); escurel records an immutable ingest event, then a
deterministic worker extracts the text, chunks it, embeds the chunks, and
materialises a single instance page whose blocks are the chunks.

Unlike the native `doc` skill (a markdown event), an `attachment` is
**read-only** in the explorer — the retained blob is the canonical
original, and the chunks/overlay are derivable from it via `rebuild`.

## How it differs from `doc`

- `doc` — a markdown artifact authored/edited in place (writable).
- `attachment` — bytes uploaded once, extracted+chunked by the backend
  (read-only; `capabilities.writable == false`).

## Optional fields

- `title` — human label carried from the upload
- `source` — where the file came from
- `about` — the typed instance this attachment most concerns
