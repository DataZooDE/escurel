import { describe, expect, it } from 'vitest';
import type { Event } from '../../src/client';
import { parsePreflightReceipt, preflightRequest, waitForPreflight } from '../../src/evolve/preflight';

const sha = 'a'.repeat(64);
const pageId = 'markdown/instances/evolve_problem/demo.md';
const receipt = (body: object): Event => ({
  event_id: 'receipt', at: null, source: 'anofox-evolve', mime: 'application/json',
  label_skill: 'evolve:preflight', instance_page_id: null, status: 'processed',
  title: 'problem-structure-checked', body: JSON.stringify(body), provenance: {},
  kind: 'system', root_event_id: 'root', run_id: null,
});

describe('Evolve problem preflight', () => {
  it('binds the request to the displayed page revision', () => {
    const request = preflightRequest(pageId, sha);
    expect(request.label_skill).toBe('evolve_preflight');
    expect(request.instance_page_id).toBe(pageId);
    expect((request.provenance as { manual: Record<string, unknown> }).manual.expected_page_sha256).toBe(sha);
  });

  it('accepts only a matching private result and reports binding issues', () => {
    const ready = receipt({ problem_sha256: sha, structural_ready_for_start: true });
    expect(parsePreflightReceipt(ready, 'root', sha)).toEqual({ ready: true });
    expect(parsePreflightReceipt(ready, 'other', sha)).toBeUndefined();
    expect(parsePreflightReceipt(ready, 'root', 'b'.repeat(64))).toBeUndefined();
    const blocked = receipt({ problem_sha256: sha, structural_ready_for_start: false,
      holdout_binding_issue: 'Holdout dates differ' });
    expect(parsePreflightReceipt(blocked, 'root', sha)).toEqual({ ready: false, issue: 'Holdout dates differ' });
    const multiple = receipt({ problem_sha256: sha, structural_ready_for_start: false,
      preflight: { issues: [{ message: 'Bad date' }, { message: 'Bad SQL' }] },
      holdout_binding_issue: 'Holdout dates differ' });
    expect(parsePreflightReceipt(multiple, 'root', sha)).toEqual({
      ready: false, issue: 'Bad date; Bad SQL; Holdout dates differ',
    });
  });

  it('waits for a final receipt rather than treating the captured request as readiness', async () => {
    let calls = 0;
    const result = await waitForPreflight({
      rootEventId: 'root', pageSha256: sha, timeoutMs: 1000,
      listEvents: async () => ({ events: ++calls === 1 ? [] : [receipt({ problem_sha256: sha, structural_ready_for_start: true })] }),
      sleep: async () => {},
    });
    expect(calls).toBe(2);
    expect(result.ready).toBe(true);
  });
});
