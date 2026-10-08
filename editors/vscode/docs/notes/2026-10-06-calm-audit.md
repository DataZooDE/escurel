# Escurel Calm audit (white skin), 2026-10-06

Sources: visual baselines (calm/ + the other three themes), and a live tour of the whole e2e suite with the
Calm theme forced (screenshots in ~/.cache/escurel-scratch/calm-audit/tour/, 42 PNG).

1. **Thread canvas connectors / arrows invisible** (thread-lanes, thread-overview, thread-canvas, 01-overview).
   Root cause: the Calm theme set `contrastBorder` (a high-contrast-only token) to the pale rule #d8d4cb; the
   canvas stroked connectors, card borders, lane dividers and some buttons with `--vscode-contrastBorder`
   whenever it was set, so on a white canvas the lines were ~1.4:1. Also: there never was an arrowhead, and
   even the stock-light fallback (40% tint of the foreground) only reached 1.8-2.3:1 (WCAG 1.4.11 wants 3:1).
   FIXED: Calm no longer sets contrastBorder; connector colour is a 70% tint of the foreground against the
   canvas (>= 3:1 in stock light, stock dark and calm, tested); each connector ends in an arrowhead
   (emphasised ones in the focus colour); lane dividers use the same tint.
2. **State chips all look the same** (every state a solid teal pill: done, open, waiting, plan ready, gave up).
   FIXED: outlined chips, green = finished, amber = pending, red = went wrong (icon + word stay).
3. **Buttons without outlines** (Show Markdown, Edit notes, Add note, Change status..., Retry/Requeue/Fix skill,
   Refresh): beige on near-white, no boundary. FIXED: Calm sets `button.border` (teal, 5.7:1); tested >= 3:1.
4. The visual harness kept the old pale contrastBorder in test/visual/tokens/calm.css (generated): regenerated
   with scripts/gen-calm-tokens.mjs; baselines of all four themes regenerated and read.
5. Overview board (overview-morning/quiet): good. Copy nit, not skin: the quiet board says "All clear" for
   "Recently finished" and "Open items" even when there is nothing yet. NOT FIXED (wording).
6. Zoomed-out overview at 40% (02d): column headers "RUN . CHANGESET" and "INSTANCES . DRAFTS" run together and
   the cards are illegible. Not Calm-specific (same in the other themes). NOT FIXED.
7. Details panel (thread-inspector-run): key/value pairs of the two columns sit at different baselines and a
   "Summary" label has an empty value. Not Calm-specific. NOT FIXED.
8. Bottom panel tabs (Problems / Output / Debug Console / Terminal / Ports) are still shown in the classic
   layout: developer clutter, not skin. Focus mode hides the rest of the chrome.
9. Status bar is a solid teal block in the classic layout (focus mode hides it): acceptable.
10. Dark / high-contrast / light baselines after the fix: connectors and arrowheads clear in all; HC keeps its
    own border colour.
