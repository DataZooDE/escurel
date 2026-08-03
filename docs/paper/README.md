# "Escurel: A Typed Knowledge Base for Agents" — paper source

A PVLDB experience-track paper on escurel: the skill–instance and
event–callback dualities, the substrate corollary that follows from them,
and an evaluation protocol centred on what typing buys an agent's context
budget.

## Review history

- **2026-08-03, codex (gpt-5.1), structure / readability / voice.** Its
  central verdict — *"the paper currently argues better than it
  demonstrates"* — is now answered inside the paper rather than around it:
  §1 ends with an explicit statement of what this draft can and cannot
  defend on the evidence it carries. Three internal contradictions it found
  were real and are fixed: the abstract claimed the same four primitives
  reach all five substrates (similarity search does not reach the two live
  ones), the contributions claimed "one embedded store for five concerns"
  (markdown, the external sources and the runner ledger all sit outside it),
  and the abstract over-scoped the guarantee register to the whole claim set
  rather than to the safety and semantic guarantees it covers. Its voice
  findings — tricolons, an author ranking his own claims, "this is not
  laziness", a repeated framing device — were applied. Its request for more
  operational history is only partly answered and remains open.
- **Self-audit before that round** caught two claims the draft had
  overstated: that the gateway schedules nothing (a reader replica does poll
  for snapshots, and the paper now names it), and that a guarantee row had
  no test (it does — the interesting part is that the test we had was
  asserting the bug).

The outline and measurement specification this is written against is
[`plan.md`](plan.md).

## Build

```bash
make          # -> paper.pdf, prints the page count and the \todonum count
make clean
```

No system LaTeX packages beyond a base `texlive` install are required. The
`acmart` class is vendored under `cls/` rather than assumed present, because
a paper that only builds on the author's machine is a paper nobody else can
check. `cls/pifont.sty` is a deliberate no-op stub, and the `\ding` glyph in
the title block is disabled for the same reason — both come from optional
font packages whose absence is a hard stop rather than a warning.

**The page count is not yet trustworthy.** `acmart` wants `libertine`,
`newtxmath` and `inconsolata`; none is installed here, so the build falls
back to Computer Modern at different metrics. Install `texlive-fontsextra`
and `texlive-fontsrecommended` before treating the 8–10 page budget as
measured.

## Status

All eight sections are drafted; the document is 8 pages. What remains is
**data, not prose**. Every unmeasured cell renders in red as `[? …]` and
`make` prints the count, so the gaps are visible in the PDF rather than only
in the source.

| Section | Prose | Data |
|---|---|---|
| Abstract, 1 Introduction | drafted | complete |
| 2 Background | drafted | complete (prototype-era, labelled) |
| 3 Two dualities and a corollary | drafted | complete |
| 4 Design and implementation | drafted | complete |
| 5 Guarantees and their verification | drafted | complete — 18/18 rows name a running test |
| 6 Evaluation | drafted | **14 cells outstanding** |
| 7 When you should not use this | drafted | complete |
| 8 Related work and summary | drafted | complete |

Outstanding measurements, in the order they should be run. Harness ids are
[`plan.md`](plan.md)'s register:

1. **H-3 — context cost at five corpus scales** (§6.2). The headline, and
   the one result the central claim stands or falls on. Needs a real-LLM
   driver and a pre-registered answer key. ~2 days.
2. **H-1 — flat-retrieval baseline** (§6.3, §6.4), plus its `k` sweep so the
   baseline is tuned rather than a straw man. ~1 day.
3. **H-9 — the five-substrate matrix** (§6.7). Real Postgres, real PDF, real
   REST service, real upstream MCP server; no mocks at the boundary the
   experiment exists to cover. ~1.5 days.
4. **H-4, H-5 — cascade throughput and kill/replay convergence** (§6.6).
   ~2 days.
5. **H-6 — skill-vs-instance growth** (§6.5). The falsifier. ~2 hours.
6. **H-2, H-7 — the labelled corpus and the ADR-0001 gate** (§6.8).
   Currently reported as an open gate, which is itself a finding.

## Rules for this manuscript

1. **No number appears here that a harness cannot reproduce.** Anything not
   yet measured is `\todonum{...}` and renders red; `make` counts them.
2. **Prototype-era numbers carry a superscript `p`** and are never presented
   as results of this system. They come from the 430-line Python
   predecessor and its verification tree.
3. **Every guarantee row in §5 cites a test that actually runs** in
   `cargo test --workspace --all-targets`. A claim whose test is `#[ignore]`d
   is not a claim.
4. **Cross-session ratios are not allowed.** Legs that are compared are
   measured in one session, on one machine, with one warm model cache.
5. **No agent-behaviour claim without a real LLM run.**
6. **Negative results stay in.** The unloadable default embedder, the BM25
   collapse under near-duplication, and the pre-deployment gate that has
   been open since May are each the natural thing to get wrong, and each is
   in the paper.
