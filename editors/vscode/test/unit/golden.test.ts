import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import type {
  EventsPage,
  ExpandResponse,
  ListInstancesResponse,
  Skill,
  UpdatePageResponse,
} from '../../src/client/types';
import { EscurelError } from '../../src/client/errors';
import { refusalFor, refusalOf } from '../../src/client';
import { skillActionViews } from '../../src/shared/actions';
import { buildPageModel } from '../../src/shared/page';
import { buildSkillPageModel } from '../../src/shared/skillPage';
import { foldRuns } from '../../src/views/runsModel';
import { buildRunView } from '../../src/runs/runModel';

// The wire, as a real gateway and runner sent it: crates/escurel-types/tests/golden/*.json, captured by
// scripts/refresh-golden.sh. The Rust types (golden_contract.rs) and the Dart kit decode the same files,
// so a wire change cannot drift between the three hand-copied implementations unnoticed. The
// annotations below are compile-time checks too: the typed shapes must still accept the golden data.
const DIR = join(__dirname, '..', '..', '..', '..', 'crates', 'escurel-types', 'tests', 'golden');
const golden = <T>(name: string): T =>
  JSON.parse(readFileSync(join(DIR, `${name}.json`), 'utf8')) as T;

describe('golden wire files', () => {
  it('list_skills: every skill decodes, and the actions are objects the host can read', () => {
    const { skills } = golden<{ skills: Skill[] }>('list_skills');
    expect(skills.length).toBeGreaterThan(0);
    for (const s of skills) {
      expect(typeof s.id).toBe('string');
      expect(Array.isArray(s.required_frontmatter)).toBe(true);
      expect(typeof s.backend.kind).toBe('string');
      expect(['auto', 'review', 'confirm', undefined]).toContain(s.autonomy);
      const model = buildSkillPageModel(s, [], []);
      expect(model.id).toBe(s.id);
      for (const a of skillActionViews(s.actions)) {
        expect(a.skill).toBeTruthy();
        expect(a.label).toBeTruthy();
      }
    }
  });

  it('expand: the page model builds from a real instance', () => {
    const e = golden<ExpandResponse>('expand_instance');
    const { skills } = golden<{ skills: Skill[] }>('list_skills');
    expect(e.page?.page_kind).toBe('instance');
    const skill = skills.find((s) => s.id === e.page!.skill)!;
    const model = buildPageModel(e, skill);
    expect(model.pageId).toBe(e.page!.page_id);
    expect(model.fields.length).toBeGreaterThan(0);
  });

  it('list_instances: the page of instances and its cursor', () => {
    const p = golden<ListInstancesResponse>('list_instances');
    expect(p.instances.length).toBeGreaterThan(0);
    expect(p.instances[0]!.page_id).toMatch(/^markdown\/instances\//);
    expect(p.next_cursor === null || typeof p.next_cursor === 'string').toBe(true);
  });

  it('a rejected write is an isError result whose issues become a typed refusal', () => {
    const r = golden<{
      isError?: boolean;
      structuredContent: Record<string, unknown>;
      content: unknown[];
    }>('refusal_result');
    expect(r.isError).toBe(true);
    const refused = refusalFor('update_page', r);
    expect(refused).toBeDefined();
    const err = EscurelError.fromPayload('update_page', refused!);
    expect(err.kind).toBe('refused');
    expect(err.issues?.length).toBeGreaterThan(0);
    expect(refusalOf(r)).toBeDefined();
  });

  it('validate: ok:false with issues is data, never a refusal', () => {
    const r = golden<{ isError?: boolean; structuredContent: { ok: boolean; issues: unknown[] } }>(
      'validate_result',
    );
    expect(r.structuredContent.ok).toBe(false);
    expect(r.structuredContent.issues.length).toBeGreaterThan(0);
    expect(refusalFor('validate', r)).toBeUndefined();
  });

  it('a held write says held_for_review (the extension must be able to read it)', () => {
    const r = golden<UpdatePageResponse & { held_for_review?: boolean }>('update_page_held');
    expect(r.held_for_review).toBe(true);
    expect(r.ok).toBe(true);
  });

  it('run events: a run folds to a finished record and a run view', () => {
    const page = golden<EventsPage>('events_run');
    const records = foldRuns(page.events, { nowMs: Date.now() });
    expect(records).toHaveLength(1);
    expect(['succeeded', 'planned', 'failed']).toContain(records[0]!.state);
    const view = buildRunView(undefined, page.events);
    expect(view.runId).toBe(records[0]!.runId);
    expect(view.status).not.toBe('running');
  });
});
