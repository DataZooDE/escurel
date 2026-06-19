# Explorer: clicking a skill in the catalogue showed an empty page

**Date:** 2026-06-19
**Area:** `escurel-explore` / `escurel_explorer_kit` (editor `CataloguePane`)

## Symptom

In the live explorer (Carl, HTTP mode), clicking a **skill** (not an
instance) in the editor catalogue opened an **empty page**. Clicking an
instance worked fine.

## Cause

`CataloguePane._SkillTile` focused the skill by setting the current page id
to the **bare** skill id:

```dart
onTap: () => ref.read(currentPageIdProvider.notifier).state = skill.id, // "team_doc"
```

`currentPageProvider` then calls `expand(id)`. On the **real server** a
skill's `page_id` is `markdown/skills/<id>.md`, so `expand("team_doc")`
matches nothing → empty page. Instances worked because
`InstanceSummary.id` is already the `page_id` (the http client maps
`page_id` → `id`), while `SkillSummary.id` is the bare id.

## Why fixture tests missed it

`FixtureEscurelClient` keys skill pages by their **bare id** (`p.id ==
"team_doc"`), so `expand("team_doc")` *succeeds* in fixture/standalone mode.
The bug only manifests against the real server's `markdown/skills/...`
page_ids — i.e. only in HTTP mode. In-process widget tests over the fixture
pass both before and after the fix, so they can't catch this class of bug.
Treat "works in fixture, empty on live" as a fixture-vs-server page_id
mismatch.

## Fix

Resolve the bare id to the real page_id first, via the existing helper the
CRM skills-menu already uses:

```dart
onTap: () => focusSkill(ref, skill.id), // resolve('[[id]]') → markdown/skills/<id>.md → focus
```

## Recognise it next time

A widget that sets `currentPageIdProvider` directly from a `SkillSummary.id`
(bare) instead of a resolved `page_id`. Skill focus must go through
`focusSkill` (or otherwise resolve `[[id]]`); only instance ids are already
page_ids.
