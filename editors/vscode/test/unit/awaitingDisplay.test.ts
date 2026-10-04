import { describe, expect, it } from 'vitest';
import { awaitingDisplay, whoWords } from '../../src/views/awaitingDisplay';
import { changesetRow, draftRow, confirmGateRow } from '../../src/views/awaitingModel';
import type { PlanRow } from '../../src/views/planRows';
import type { Changeset, Draft, Event } from '../../src/client';

const NOW = Date.parse('2026-10-04T12:00:00Z');
const at = '2026-10-04T11:57:00Z';

describe('awaitingDisplay: one structure for every kind of row', () => {
  it('a changeset reads "<page> — Proposed changes" with count, who and when', () => {
    const cs = {
      changeset_id: 'CS1',
      target_page_ids: [
        'markdown/instances/customer-order__order-4500131.md',
        'markdown/instances/x__y.md',
      ],
      drafts: 2,
      author: 'agent:supplier-risk',
      created_at: at,
      status: 'open',
    } as unknown as Changeset;
    expect(awaitingDisplay(changesetRow(cs), NOW)).toMatchObject({
      label: 'order-4500131 +1 — Proposed changes',
      description: '2 changes · supplier-risk agent · 3 m',
    });
  });

  it('a draft reads "<page> — Proposed change", a person by name', () => {
    const d = {
      draft_id: 'D1',
      target_page_id: 'markdown/instances/supplier-rating__nordform.md',
      author: 'alice',
      created_at: at,
      status: 'open',
    } as unknown as Draft;
    expect(awaitingDisplay(draftRow(d), NOW)).toMatchObject({
      label: 'nordform — Proposed change',
      description: 'alice · 3 m',
    });
  });

  it('a confirm gate and a plan use the same shape', () => {
    const ev = {
      event_id: 'E1',
      title: 'Confirm notice',
      label_skill: 'customer-notice',
      at,
      status: 'inbox',
    } as unknown as Event;
    expect(awaitingDisplay(confirmGateRow(ev), NOW)).toMatchObject({
      label: 'Confirm notice — Needs your confirmation',
      description: 'customer-notice · 3 m',
    });
    const plan: PlanRow = {
      kind: 'plan',
      id: 'plan:R1',
      label: 'Plan ready · supplier-risk on order-4500131',
      description: 'Approve plan',
      timestamp: at,
      runId: 'R1',
      rootEventId: undefined,
      skill: 'supplier-risk',
      pageId: 'markdown/instances/customer-order__order-4500131.md',
    };
    expect(awaitingDisplay(plan, NOW)).toMatchObject({
      label: 'order-4500131 — Plan to approve',
      description: 'supplier-risk · 3 m',
    });
  });
});

describe('whoWords', () => {
  it('says an agent as an agent', () => {
    expect(whoWords('agent:supplier-risk')).toBe('supplier-risk agent');
    expect(whoWords('agt:lead-scorer')).toBe('lead-scorer agent');
    expect(whoWords('alice')).toBe('alice');
    expect(whoWords(undefined)).toBe('');
  });
});
