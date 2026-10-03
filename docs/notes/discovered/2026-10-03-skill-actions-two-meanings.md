# `actions:` meant two different things in two systems

**Symptom.** Planning the supplier-risk-analysis demo, a skill's `actions:` was a list of skill ids in escurel
(the runner's cascade allow-list; validated as `action_skill_unknown`) but a list of OBJECTS
`{name, kind: prompt|event, label, ...}` in Peacock's document view (buttons). The same key, two shapes; neither
side tolerated the other's.

**Decision / fix.** Peacock's object form is canonical, objects only (a bare id is `action_invalid`). The cascade
allow-list is now derived: the `event` skills of the `kind: event` entries; `kind: prompt` entries restrict nothing.
`list_skills` carries `actions: [{name, kind, label, event?, prompt?}]`; the `title`/`body` templates stay off the wire
(Peacock substitutes them server-side). See `.claude/skills/escurel-platform` 0.7.0.

**Recognise it next time.** A consumer page with `actions: [some-skill]` now fails `validate` with `action_invalid`;
fix it by writing the object. A key shared across repos (escurel, peacock, the VS Code extension) needs one owner of
its shape: check `../peacock/.claude/skills/peacock-platform` before changing it. The pre-push hook runs the whole
workspace, so an unmigrated fixture in another crate fails the push, not just the crate you edited.
