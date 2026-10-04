---
kind: skill
id: supplier-document
title: Supplier document
folder: purchasing/documents
role: record
tags: [document, contract]
description: A supplier's contract or terms, uploaded as a file. Its text is split into chunks you can search and read here; the original file is kept.
autonomy: review
backend:
  kind: document
  accepts: [text/markdown, text/plain]
  chunk: {max_chars: 380, overlap: 40}
---

# supplier-document

One instance per uploaded document. Nothing is typed in by hand: the file is uploaded (the demo uploads
one when it starts), its text is extracted and chunked, and the original stays retrievable. The page
shows the first chunks read-only, with a button for the original file.
