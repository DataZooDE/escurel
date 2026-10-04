import { describe, expect, it } from 'vitest';
import type { Event, Skill } from '../../src/client/types';
import { foldRuns, tooltipFor, runLabel } from '../../src/views/runsModel';
import { buildRunView, mergeToolCallPage } from '../../src/runs/runModel';
import { skillFacts } from '../../src/shared/freshness';
import { buildSkillPageModel } from '../../src/shared/skillPage';
import { inboxRow } from '../../src/views/inboxModel';
import { changesetRow, draftRow, confirmGateRow } from '../../src/views/awaitingModel';
import { pageSlug } from '../../src/shared/pageId';
import { foldLineage, toThreadView } from '../../src/thread/threadModel';

// Text from a run, a skill or a page is data from another person. Every model that feeds a label, a
// tooltip or a webview must hand out text with no bidi/zero-width/control characters and a bounded size.
const EVIL = `\u202Eexe.txt\u200B\u0000\u2066x\u2069${'A'.repeat(2_000_000)}`;
// eslint-disable-next-line no-control-regex
const UNSAFE = /[\u0000-\u0008\u000b-\u001f\u007f-\u009f\u200b-\u200f\u202a-\u202e\u2060-\u2064\u2066-\u2069\ufeff]/;

function walk(v: unknown, out: string[] = []): string[] {
  if (typeof v === 'string') out.push(v);
  else if (Array.isArray(v)) v.forEach((x) => walk(x, out));
  else if (v && typeof v === 'object') Object.values(v).forEach((x) => walk(x, out));
  return out;
}
function expectClean(v: unknown, cap = 5000): void {
  for (const s of walk(v)) {
    expect(UNSAFE.test(s)).toBe(false);
    expect(s.length).toBeLessThanOrEqual(cap);
  }
}

const sys = (title: string, body: unknown, extra: Partial<Event> = {}): Event =>
  ({
    event_id: `run:R1:${title}`,
    kind: 'system',
    label_skill: 'escurel:run',
    run_id: 'R1',
    title,
    at: '2026-01-01T00:00:00Z',
    body: JSON.stringify(body),
    ...extra,
  }) as Event;

describe('untrusted text is cleaned where it enters the models', () => {
  it('foldRuns: reason, error, summary, skill, page, harness', () => {
    const events = [
      sys('run-started', {}, {
        instance_page_id: `instances/x/${EVIL}.md`,
        provenance: { runner: { event_id: 'T1', harness: EVIL } },
      }),
      sys('run-finished', { status: 'failed', reason: EVIL, error: EVIL, summary: EVIL }),
    ];
    const recs = foldRuns(events, { nowMs: 0, skillByEvent: new Map([['T1', EVIL]]) });
    expectClean(recs, 1000);
    expectClean([tooltipFor(recs[0]!, 0), runLabel(recs[0]!)], 5000);
  });

  it('buildRunView: attempt error, summary, plan steps, harness, target', () => {
    const view = buildRunView(undefined, [
      sys('run-attempt', { attempt: 1, outcome: EVIL, error: EVIL }),
      sys('run-finished', {
        status: 'processed',
        summary: EVIL,
        harness: EVIL,
        plan: [{ step: EVIL, status: 'pending' }],
        target_page_id: EVIL,
      }),
    ]);
    expectClean(view, 5000);
    const merged = mergeToolCallPage(view, {
      calls: [
        {
          seq: 1,
          tool: EVIL,
          status: EVIL,
          error_code: EVIL,
          at: '2026-01-01T00:00:00Z',
          duration_ms: 1,
          request_bytes: 1,
          response_bytes: 1,
        },
      ],
      next_after: null,
    } as never);
    expectClean(merged.calls, 400);
  });

  it('skill OKF strings and skill page facts', () => {
    const skill = {
      id: 's',
      verified: EVIL,
      generated: EVIL,
      status: EVIL,
      stale_after: EVIL,
      description: EVIL,
      role: EVIL,
      folder: EVIL,
      tags: [EVIL],
      resource: EVIL,
      backend: { kind: 'none' },
      layer: 'overlay',
      required_frontmatter: [],
      optional_frontmatter: [],
      fields: [{ name: 'f', kind: 'enum', values: [EVIL], required: false, description: EVIL, label: EVIL }],
    } as unknown as Skill;
    expectClean(skillFacts(skill, 0).facts, 700);
    expectClean(buildSkillPageModel(skill, [], []), 5000);
  });

  it('inbox, awaiting and slugs', () => {
    const ev = { event_id: 'E', label_skill: EVIL, title: EVIL, instance_page_id: `i/${EVIL}.md`, status: 'inbox' } as Event;
    // Display fields only: the row also carries the raw event, which commands address by id.
    const shown = (r: { label: string; description: string; tooltip?: string }) => [r.label, r.description, r.tooltip ?? ''];
    expectClean(shown(inboxRow(ev)), 3000);
    expectClean(shown(confirmGateRow(ev)), 3000);
    expectClean(
      [
        changesetRow({ changeset_id: 'C', target_page_ids: [EVIL], drafts: 1, author: EVIL, status: 'open' } as never).label,
        changesetRow({ changeset_id: 'C', target_page_ids: [EVIL], drafts: 1, author: EVIL, status: 'open' } as never).description,
        draftRow({ draft_id: 'D', target_page_id: EVIL, author: EVIL, status: 'open' } as never).description,
        pageSlug(EVIL),
      ],
      700,
    );
  });

  it('thread cards', () => {
    const folded = foldLineage([
      {
        root_event_id: 'R',
        nodes: [
          { id: 'R', type: 'event', label_skill: EVIL, title: EVIL, parent: null, state: 'processed', instance_page_id: EVIL },
          { id: 'RUN', type: 'run', parent: 'R', state: 'failed', summary: EVIL, harness: EVIL, target_page_id: EVIL },
        ],
      } as never,
    ]);
    expectClean(toThreadView(folded).nodes, 3000);
  });
});
