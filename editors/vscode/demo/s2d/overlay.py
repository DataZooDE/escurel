#!/usr/bin/env python3
"""The demo overlay on the migrated S2D pages (what the editor needs that the lab seed does not say).

Only frontmatter is touched; bodies stay as the source wrote them. Kept deliberately small and
explicit so it is clear what differs from the single source:

* a place in the Knowledge tree (`folder`, `role`, `tags`, `title`), which the lab (a chat surface)
  has no use for;
* `autonomy: review` on the four record skills, so a machine run's write is HELD in Awaiting you and
  promoting it is the planner's approval (the lab's agent records directly and asks in chat);
* the record skills' `viewer:` points at the analytic report that explains the record (the lab's
  `viewer:` points at a report that only repeats the record, which this page already is);
* `qty_needed` becomes an optional key of `exception_resolution` (the options report takes it).
"""
import sys
from pathlib import Path

import yaml

PAGES = Path(sys.argv[1])

DATA = {"exception_exposure": "Open lots and the orders they feed",
        "sourcing_options": "Recovery options per material",
        "outbound_transports": "Outbound transports",
        "spare_parts": "Spare parts to end of service"}
RECORDS = {
    "supplier_exception": ("Supplier exception", "logistics/source", "exception-impact-report"),
    "exception_resolution": ("Exception resolution", "logistics/source", "resolution-options-report"),
    "transport_plan": ("Transport plan", "logistics/deliver", "consolidation-plan-report"),
    "ltb_decision": ("Last-time-buy decision", "logistics/after-sales", "ltb-report"),
}
REPORTS = {
    "exception-impact-report": ("Impact of a supplier delay", "logistics/source"),
    "resolution-options-report": ("Recovery options", "logistics/source"),
    "consolidation-plan-report": ("Consolidation plan", "logistics/deliver"),
    "consolidation-report": ("Consolidation potential", "logistics/deliver"),
    "ltb-report": ("Last-time-buy view", "logistics/after-sales"),
    "supplier-exception-report": ("Supplier exception report", "logistics/source"),
    "resolution-report": ("Resolution report", "logistics/source"),
    "transport-plan-report": ("Transport plan report", "logistics/deliver"),
    "ltb-decision-report": ("Last-time-buy report", "logistics/after-sales"),
}


def edit(path: Path, change) -> None:
    text = path.read_text()
    _, fm, body = text.split("---\n", 2)
    meta = yaml.safe_load(fm)
    change(meta)
    path.write_text("---\n" + yaml.dump(meta, sort_keys=False, allow_unicode=True, width=1000) + "---\n" + body)


def data(meta, title):
    meta.update({"title": title, "folder": "logistics/data", "role": "helper", "tags": ["s2d", "data"]})


def record(meta, title, folder, report):
    meta.update({"title": title, "folder": folder, "role": "record", "tags": ["s2d"], "autonomy": "review",
                 "viewer": {"report": report}})
    if meta["id"] == "exception_resolution":
        meta["optional_frontmatter"] = list(meta["optional_frontmatter"]) + ["qty_needed"]


def report(meta, title, folder):
    meta.update({"title": title, "folder": folder, "role": "report", "tags": ["s2d", "report"]})


for sid, title in DATA.items():
    edit(PAGES / "skills" / f"{sid}.md", lambda m, t=title: data(m, t))
for sid, (title, folder, rep) in RECORDS.items():
    edit(PAGES / "skills" / f"{sid}.md", lambda m, a=(title, folder, rep): record(m, *a))
for sid, (title, folder) in REPORTS.items():
    edit(PAGES / "reports" / f"{sid}.md", lambda m, a=(title, folder): report(m, *a))
for q in (PAGES / "queries").glob("*.md"):
    edit(q, lambda m: m.update({"title": m["id"].replace("_", " ").capitalize()}))
