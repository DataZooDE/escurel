import { describe, expect, it } from 'vitest';
import {
  parseProposedValue,
  buildProposal,
  describeWriteBackRefusal,
  latestWriteBack,
  describeCurrent,
  writeBackLine,
} from '../../src/shared/writeBack';
import type { Event } from '../../src/client/types';

const ev = (over: Omit<Partial<Event>, 'body'> & { body: unknown }): Event => ({
  event_id: 'write-back:d1:applied',
  at: '2026-10-03T12:05:00.000000Z',
  source: 'escurel',
  mime: 'application/json',
  label_skill: 'escurel:write-back',
  instance_page_id: 'markdown/instances/customer/c-0001.md',
  status: 'processed',
  title: 'write-back-applied',
  kind: 'system',
  provenance: null,
  root_event_id: null,
  run_id: null,
  ...over,
  body: JSON.stringify(over.body),
});

describe('buildProposal', () => {
  it('is the notes plus a reserved write_back block naming the change and what it was based on', () => {
    const md = buildProposal({
      pageId: 'markdown/instances/customer/c-0001.md',
      skill: 'customer',
      field: 'tier',
      value: 'gold',
      baseEtag: 'w1:abc',
      notes: 'Upgraded after the renewal call.',
    });
    expect(md).toBe(
      '---\nkind: instance\nid: "c-0001"\nskill: "customer"\nwrite_back:\n  patch: { "tier": "gold" }\n  base_etag: "w1:abc"\n---\nUpgraded after the renewal call.\n',
    );
  });

  it('quotes a value safely: a colon, a quote or a newline cannot break out of the block', () => {
    const md = buildProposal({
      pageId: 'markdown/instances/customer/c-0001.md',
      skill: 'customer',
      field: 'tier',
      value: 'gold"\n  evil: true',
      baseEtag: 'w1:x',
      notes: '',
    });
    expect(md).toContain('patch: { "tier": "gold\\"\\n  evil: true" }');
    expect(md.split('\n').filter((l) => l.startsWith('evil'))).toEqual([]);
  });

  it('keeps a number or a boolean a number or a boolean', () => {
    const base = {
      pageId: 'markdown/instances/o/o-1.md',
      skill: 'o',
      baseEtag: 'e',
      notes: '',
    };
    expect(buildProposal({ ...base, field: 'qty', value: 5 })).toContain('patch: { "qty": 5 }');
    expect(buildProposal({ ...base, field: 'ok', value: true })).toContain('patch: { "ok": true }');
  });
});

describe('latestWriteBack', () => {
  it('is the newest outcome, and applying alone is "in progress"', () => {
    const events = [
      ev({
        event_id: 'write-back:d1:applying',
        at: '2026-10-03T12:04:59Z',
        body: { outcome: 'applying', draft_id: 'd1' },
      }),
      ev({ body: { outcome: 'applied', draft_id: 'd1', attempts: 2 } }),
    ];
    expect(latestWriteBack(events)).toEqual({
      outcome: 'applied',
      at: '2026-10-03T12:05:00.000000Z',
      draftId: 'd1',
      attempts: 2,
    });
    expect(latestWriteBack([events[0]!])?.outcome).toBe('applying');
  });

  it('a finished change is never reported as still in progress, whatever order the events come in', () => {
    // The gateway writes `applying` and then the outcome within the same instant, so they can carry
    // the very same timestamp; the live demo showed "being sent" for a change that had been applied.
    const at = '2026-10-03T12:05:00.000000Z';
    const applying = ev({
      event_id: 'write-back:d1:applying',
      at,
      body: { outcome: 'applying', draft_id: 'd1' },
    });
    const applied = ev({ at, body: { outcome: 'applied', draft_id: 'd1', attempts: 1 } });
    expect(latestWriteBack([applied, applying])?.outcome).toBe('applied'); // newest first
    expect(latestWriteBack([applying, applied])?.outcome).toBe('applied'); // oldest first
  });

  it('ignores other events, and an event whose body is not a write-back outcome', () => {
    expect(latestWriteBack([])).toBeUndefined();
    expect(
      latestWriteBack([ev({ label_skill: 'escurel:review', body: { outcome: 'applied' } })]),
    ).toBeUndefined();
    expect(latestWriteBack([{ ...ev({ body: {} }), body: 'not json' }])).toBeUndefined();
    expect(latestWriteBack([ev({ body: { outcome: 'weird' } })])).toBeUndefined();
  });
});

describe('writeBackLine', () => {
  it('says what happened in words a person can act on', () => {
    const at = '2026-10-03T12:05:00.000000Z';
    expect(writeBackLine({ outcome: 'applied', at, draftId: 'd', attempts: 1 })).toBe(
      'Last change sent to the source at 12:05 UTC: applied.',
    );
    expect(writeBackLine({ outcome: 'failed', at, draftId: 'd', attempts: 3 })).toBe(
      'Last change to the source at 12:05 UTC could not be sent after 3 attempts. Promote it again from Awaiting You to retry.',
    );
    expect(writeBackLine({ outcome: 'rejected', at, draftId: 'd', attempts: 1 })).toBe(
      'Last change to the source at 12:05 UTC was rejected by it. Propose it again with a different value.',
    );
    expect(writeBackLine({ outcome: 'conflict', at, draftId: 'd', attempts: 1 })).toBe(
      'Last change to the source at 12:05 UTC conflicted: the row had changed. Read it again and propose again.',
    );
    expect(writeBackLine({ outcome: 'applying', at, draftId: 'd', attempts: 0 })).toBe(
      'A change is being sent to the source (since 12:05 UTC).',
    );
  });
});

describe('describeWriteBackRefusal', () => {
  it('turns the gateway codes into plain advice', () => {
    expect(describeWriteBackRefusal('write_back_conflict', 'x')).toMatch(/changed upstream/i);
    expect(describeWriteBackRefusal('write_back_failed', 'could not be reached')).toMatch(
      /Write-back failed/,
    );
    expect(describeWriteBackRefusal('write_back_rejected', 'rejected')).toMatch(/rejected/);
    expect(describeWriteBackRefusal('write_back_unknown_outcome', 'x')).toMatch(
      /not repeated|operator|check the source/i,
    );
    expect(
      describeWriteBackRefusal(
        'backend_read_only_field',
        '`display_name` is not a writable column of `customer`.',
      ),
    ).toMatch(/not a writable column|read-only/i);
    expect(describeWriteBackRefusal('something_else', 'plain message')).toBeUndefined();
  });
});

describe('writeBackLine when nothing was sent', () => {
  it('says the source could not be reached, not "after 0 attempts"', () => {
    const line = writeBackLine({
      outcome: 'failed',
      at: '2026-10-03T12:05:00.000000Z',
      draftId: 'd1',
      attempts: 0,
    });
    expect(line).toMatch(/did not go through/);
    expect(line).toMatch(/could not be reached/);
    expect(line).not.toMatch(/0 attempts/);
    expect(line).toMatch(/Promote it again/);
  });
});

describe('parseProposedValue', () => {
  it('keeps the type the field has: a number stays a number, a boolean a boolean, text text', () => {
    expect(parseProposedValue('gold', 'silver')).toEqual({ ok: true, value: 'gold' });
    expect(parseProposedValue(' 42.5 ', 10)).toEqual({ ok: true, value: 42.5 });
    expect(parseProposedValue('true', false)).toEqual({ ok: true, value: true });
    expect(parseProposedValue('No', true)).toEqual({ ok: true, value: false });
  });

  it('refuses what cannot be that type, and an empty value', () => {
    expect(parseProposedValue('abc', 10)).toEqual({ ok: false, error: 'Enter a number.' });
    expect(parseProposedValue('maybe', true)).toEqual({ ok: false, error: 'Enter true or false.' });
    expect(parseProposedValue('   ', 'x')).toEqual({ ok: false, error: 'Enter a value.' });
  });

  it('refuses a value the gateway would have to guess about', () => {
    expect(parseProposedValue('a'.repeat(2001), 'x')).toEqual({
      ok: false,
      error: 'That is too long (limit 2000 characters).',
    });
  });
});

// The names come from the gateway (a column's name, a skill id, a page id). They are put into YAML, so
// each must be quoted or refused: a column named `a, b: x` must not add a key to the reserved block.
describe('buildProposal: names from the gateway cannot break out of the YAML', () => {
  const base = {
    pageId: 'markdown/instances/customer/c-0001.md',
    skill: 'customer',
    field: 'tier',
    value: 'gold',
    baseEtag: 'w1:abc',
    notes: 'n',
  };

  it('refuses a column that is not a plain identifier', () => {
    for (const field of ['a, b: x', 'x}', 'a\nb', 'a b', '', 'a:b', '"q"', '__proto__ ']) {
      expect(() => buildProposal({ ...base, field }), JSON.stringify(field)).toThrow(/column/i);
    }
  });

  it('quotes the skill and the id, so a hostile one stays one scalar', () => {
    const md = buildProposal({
      ...base,
      skill: 'x\nwrite_back:\n  patch: { evil: 1 }',
      pageId: 'markdown/instances/customer/c: {a}.md',
    });
    const lines = md.split('\n');
    expect(lines.filter((l) => l.startsWith('write_back:')).length).toBe(1);
    expect(md).toContain('skill: "x\\nwrite_back:\\n  patch: { evil: 1 }"');
    expect(md).toContain('id: "c: {a}"');
  });
});

describe('describeCurrent: the value shown in the prompt comes from the source and is untrusted', () => {
  it('shows an empty value as such, a short value as is, and bounds a long or hostile one', () => {
    expect(describeCurrent(undefined)).toBe('(empty)');
    expect(describeCurrent('B')).toBe('B');
    expect(describeCurrent(42)).toBe('42');
    const long = describeCurrent(`x\u202E${'y'.repeat(5000)}`);
    expect(long.length).toBeLessThanOrEqual(120);
    expect(long).not.toContain('\u202E');
    expect(describeCurrent({ a: 1 })).toBe('{"a":1}');
  });
});

describe('write-back YAML hardening', () => {
  const base = {
    pageId: 'markdown/instances/customer/c-0001.md',
    skill: 'customer',
    baseEtag: 'e',
    notes: '',
  };
  it('writes U+0085, U+2028 and U+2029 as escapes (YAML would fold them as line breaks)', () => {
    const md = buildProposal({ ...base, field: 'tier', value: 'a\u2028b\u0085c\u2029d' });
    expect(md).toContain('"a\\u2028b\\u0085c\\u2029d"');
    expect(/[\u0085\u2028\u2029]/.test(md)).toBe(false);
  });
  it('types an empty numeric or boolean column by its declared kind', () => {
    expect(parseProposedValue('42', '', 'int')).toEqual({ ok: true, value: 42 });
    expect(parseProposedValue('4.5', null, 'float')).toEqual({ ok: true, value: 4.5 });
    expect(parseProposedValue('yes', undefined, 'bool')).toEqual({ ok: true, value: true });
    expect(parseProposedValue('abc', '', 'int')).toEqual({ ok: false, error: 'Enter a number.' });
    expect(parseProposedValue('42', '', 'string')).toEqual({ ok: true, value: '42' });
    expect(parseProposedValue('42', 'text-now', 'int')).toEqual({ ok: true, value: '42' });
  });
});
