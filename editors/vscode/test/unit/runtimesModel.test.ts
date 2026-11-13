import { describe, expect, it } from 'vitest';
import type { Runtime } from '../../src/client/types';
import { heldBy, requiresWords, runtimeRows } from '../../src/views/runtimesModel';

// `list_runtimes` (references/02, #818): one row per registered runner, its liveness joined from
// the runner's own status record.
const online: Runtime = {
  runner_id: 'laptop',
  page_id: 'markdown/instances/runtime/laptop.md',
  harnesses: [
    { name: 'echo', version: null },
    { name: 'claude', version: '2.1.3 (Claude Code)' },
  ],
  labels: ['repo:escurel', 'site:muc'],
  max_concurrent: 2,
  claim_holder: 'svc-runner#01abc',
  status: 'online',
  last_seen: '2026-11-13T10:00:00Z',
  age_s: 12,
  stale_after_s: 90,
  version: '2026.11.13',
  live_runs: 1,
};
const stale: Runtime = {
  ...online,
  runner_id: 'build-box',
  page_id: 'markdown/instances/runtime/build-box.md',
  harnesses: [{ name: 'codex', version: 'codex-cli 0.168.0' }],
  labels: [],
  max_concurrent: null,
  claim_holder: 'svc-runner#02def',
  status: 'stale',
  last_seen: '2026-11-13T09:40:00Z',
  age_s: 1200,
  live_runs: 0,
};
const now = new Date('2026-11-13T10:00:12Z');

describe('the Runtimes view rows', () => {
  it('say the status in words, then what the runner offers and is doing', () => {
    const [box, laptop] = runtimeRows([online, stale], now);
    expect(laptop).toMatchObject({
      id: online.page_id,
      label: 'laptop',
      status: 'online',
      pageId: online.page_id,
      description:
        'online · echo, claude 2.1.3 (Claude Code) · repo:escurel, site:muc · 1 live run',
    });
    expect(box?.description).toBe('stale · codex codex-cli 0.168.0 · no labels · idle');
  });

  it('carry when the runner was last seen and the page in the tooltip', () => {
    const [box, laptop] = runtimeRows([online, stale], now);
    expect(laptop?.tooltip).toContain(
      'Online: last seen just now (stale after 1 min 30 s of silence)',
    );
    expect(laptop?.tooltip).toContain('At most 2 runs at once');
    expect(laptop?.tooltip).toContain('Runner 2026.11.13');
    expect(laptop?.tooltip).toContain('Page: markdown/instances/runtime/laptop.md');
    expect(box?.tooltip).toContain(
      'Stale: last seen 20 min ago (stale after 1 min 30 s of silence). It takes no work until it reports again.',
    );
  });

  it('word a stopped runner and one that never reported', () => {
    const rows = runtimeRows(
      [
        { ...online, runner_id: 'a', status: 'stopped', live_runs: 0 },
        {
          ...online,
          runner_id: 'b',
          status: 'unknown',
          last_seen: null,
          age_s: null,
          live_runs: 0,
        },
      ],
      now,
    );
    expect(rows.map((r) => r.description.split(' · ')[0])).toEqual(['stopped', 'not reporting']);
    expect(rows[0]?.tooltip).toContain('Stopped: it said it was shutting down');
    expect(rows[1]?.tooltip).toContain('Not reporting: no status record from this runner yet');
  });

  it('sort by runner id and clean what the runner wrote', () => {
    const rows = runtimeRows([{ ...online, runner_id: 'zz‮' }, stale], now);
    expect(rows.map((r) => r.label)).toEqual(['build-box', 'zz']);
  });
});

describe('a member’s read (no CLI versions, no claim holders)', () => {
  const member = (r: Runtime): Runtime => {
    const { claim_holder, ...rest } = r;
    void claim_holder;
    return { ...rest, harnesses: r.harnesses.map(({ name }) => ({ name })) };
  };

  it('lists the harnesses by name', () => {
    const [box, laptop] = runtimeRows([member(online), member(stale)], now);
    expect(laptop?.description).toBe('online · echo, claude · repo:escurel, site:muc · 1 live run');
    expect(box?.tooltip).toContain('Harnesses: codex');
  });

  it('says a runner holds the event without naming one', () => {
    const claim = { holder: 'svc-runner#02def', expires_at: '2026-11-13T10:01:00Z' };
    expect(heldBy(claim, [member(online), member(stale)])).toEqual({ until: claim.expires_at });
    expect(heldBy(claim, [])).toEqual({ until: claim.expires_at });
  });
});

describe('a skill’s requires in words', () => {
  it('names the harness and the labels', () => {
    expect(requiresWords({ harness: 'codex', labels: ['repo:escurel'] })).toEqual({
      chip: 'needs codex · repo:escurel',
      detail: 'needs harness codex · labels repo:escurel',
    });
    expect(requiresWords({ labels: ['site:muc', 'has:sap-gui'] })?.chip).toBe(
      'needs site:muc, has:sap-gui',
    );
  });

  it('says a malformed requires blocks routing, and says nothing when absent', () => {
    expect(requiresWords({ invalid: true })).toEqual({
      chip: 'routing blocked',
      detail: 'Routing blocked: requires is malformed, so no runner takes this work',
    });
    expect(requiresWords(undefined)).toBeUndefined();
    expect(requiresWords({})).toBeUndefined();
  });
});

describe('which runtime holds an event', () => {
  const claim = { holder: 'svc-runner#02def', expires_at: '2026-11-13T10:01:00Z' };

  it('maps the lease holder to the runtime that carries it', () => {
    expect(heldBy(claim, [online, stale])).toEqual({
      runtime: 'build-box',
      pageId: stale.page_id,
      until: claim.expires_at,
    });
  });

  it('falls back to the holder when no runtime is registered for it', () => {
    expect(heldBy({ ...claim, holder: 'svc-runner#zzz' }, [online])).toEqual({
      runtime: 'svc-runner#zzz',
      until: claim.expires_at,
    });
    expect(heldBy(undefined, [online])).toBeUndefined();
  });
});
