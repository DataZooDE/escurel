# "Typed Memory for Autonomous Agents" — paper source

**Formulation: IEEE Transactions on Artificial Intelligence, position /
innovation paper.** The earlier PVLDB experience-track formulation of the same
material is in history at `1c1372f` and can be restored from it. The content
is largely shared; what changed is the register (argument-first rather than
project-narrative), the addition of a section delimiting what is and is not
novel against the knowledge-representation literature, and a narrowing of
several empirical claims.

`cls/IEEEtran.cls` carries a **local modification**: upstream selects Times
and Helvetica and calls `\normalfont\selectfont` at class-load time, which is
fatal on a machine without `texlive-fontsrecommended`. The vendored copy
substitutes Computer Modern; see the note at the head of the file. Restore the
pristine class for camera-ready, and re-check the page count, since metrics
change.

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

All eight sections are drafted; the document is 10 pages; **`make` reports 0
unmeasured cells**. Every number in it was produced by a harness in this
repository, and the four experiments we could not run are named in the paper
with the reason.

| Section | Prose | Data |
|---|---|---|
| Abstract, 1 Introduction | drafted | complete |
| 2 Background | drafted | complete (prototype-era, labelled) |
| 3 Two dualities and a corollary | drafted | complete |
| 4 Design and implementation | drafted | complete |
| 5 Guarantees | drafted | 18/18 rows name a running test |
| 6.2 Tier-1 cost | drafted | measured (`data/tier1.json`) |
| 6.3 Retrieval ladder | drafted | measured (`data/scifact.json`) |
| 6.6 Projection loop + replay | drafted | measured (`data/{cascade,replay}.json`) |
| 6.7 Five substrates | drafted | measured (`data/substrates.json`) |
| 7 When you should not use this | drafted | complete |
| 8 Related work and summary | drafted | complete |

## What is deliberately absent

Four experiments were specified and not run. Each is named in the paper with
its reason, because an unrun experiment reported honestly is worth more than
a plausible number:

1. **Agent behaviour under a real model** (§6.2's second half). Whether a
   language model actually exploits the cheap tier is the behavioural claim,
   and the three-arm comparison that would settle it was not built. The
   predecessor's 12.4x is prototype context, not a result for this system.
2. **Typed-vs-flat retrieval and the type/filter ablation** (§6.4). Not
   runnable on a public IR benchmark: SciFact is single-skill, so a skill
   filter compares a query against itself. Needs a multi-skill labelled
   corpus.
3. **Skill growth against instance growth** (§6.5) — the falsifier. Needs
   production corpus history that is not on this machine. We declined to
   draw the curve from fixtures.
4. **The ADR-0001 storage gate** (§6.8). Its 460-block fixture was never
   versioned with the code and no longer exists.

Reruns: the harnesses are `substrate_matrix.rs` (escurel-server,
`--features live-substrates`), `projection_measurements.rs` (escurel-runner,
`--features paper-measurements`), `tier1_cost.rs` (escurel-index, same
feature), and `escurel-eval` over `datasets/scifact`.

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
