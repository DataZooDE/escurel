import { describe, expect, it } from 'vitest';
import { EscurelError } from '../../src/client/errors';
import { checkSkillsCompatible } from '../../src/client/compat';
import { connectionStateOf, describeError } from '../../src/errors';

// What the gateway now answers (a tenant waiting for its data migration, a rate limit, a refusal) used to
// reach the person as `rpc: tenant_quarantined: …` or `forbidden: HTTP 403`. Each is a sentence with a
// next step, and an OLD gateway is told apart from a broken one.
describe('error kinds the gateway emits', () => {
  it('maps a quarantined tenant, from the JSON-RPC data.code and from a 403 body', () => {
    const rpc = EscurelError.fromRpc(
      'list_skills',
      -32602,
      'tenant_quarantined: tenant `acme` has 2 page(s)…',
      {
        code: 'tenant_quarantined',
        retryable: false,
      },
    );
    expect(rpc.kind).toBe('tenant_quarantined');
    expect(EscurelError.fromHttp(403, { error: 'tenant_quarantined' }).kind).toBe(
      'tenant_quarantined',
    );
  });

  it('says what to do about each', () => {
    const q = describeError(new EscurelError('tenant_quarantined', 'x'));
    expect(q).toMatch(/administrator/i);
    expect(q).not.toContain('type:');
    expect(q).toContain('escurel admin migrate-kind');
    expect(describeError(new EscurelError('forbidden', 'HTTP 403'))).toMatch(/not allowed|access/i);
    expect(describeError(new EscurelError('quota_exhausted', 'HTTP 429'))).toMatch(
      /rate|limit|try again/i,
    );
    expect(describeError(new EscurelError('tenant_suspended', 'x'))).toMatch(/suspended/i);
    expect(describeError(new EscurelError('server_incompatible', 'x'))).toMatch(/older|update/i);
    for (const kind of [
      'forbidden',
      'quota_exhausted',
      'tenant_suspended',
      'tenant_quarantined',
    ] as const) {
      expect(describeError(new EscurelError(kind, 'x')), kind).not.toMatch(
        /^(rpc|forbidden|quota_exhausted):/,
      );
    }
  });

  it('classifies an error for the views: unauthorized, quarantined, incompatible, unreachable', () => {
    expect(connectionStateOf(new EscurelError('unauthorized', 'x'))).toBe('unauthorized');
    expect(connectionStateOf(new EscurelError('tenant_quarantined', 'x'))).toBe('quarantined');
    expect(connectionStateOf(new EscurelError('server_incompatible', 'x'))).toBe('incompatible');
    expect(connectionStateOf(new EscurelError('transport', 'x'))).toBe('unreachable');
    expect(connectionStateOf(new EscurelError('not_found', 'x'))).toBe('error');
    expect(connectionStateOf(new Error('boom'))).toBe('error');
  });
});

// A gateway from before skill actions became objects returns `actions: ["supplier-risk"]`; the extension
// then crashed in rendering with `t.replace is not a function` and showed a blank view.
describe('an old gateway', () => {
  it('is reported as incompatible, with a sentence, before any view renders from it', () => {
    const old = [{ id: 'customer-order', actions: ['supplier-risk'] }] as never;
    expect(() => checkSkillsCompatible(old)).toThrowError(EscurelError);
    try {
      checkSkillsCompatible(old);
    } catch (e) {
      expect((e as EscurelError).kind).toBe('server_incompatible');
      expect(describeError(e)).toMatch(/older than this extension/i);
    }
  });

  it('lets a current gateway through untouched', () => {
    const ok = [
      { id: 'customer-order', actions: [{ name: 'a', kind: 'event', label: 'A', event: 's' }] },
      { id: 'x' },
    ] as never;
    expect(checkSkillsCompatible(ok)).toBe(ok);
  });
});

describe('describeError never passes control or bidi characters or an unbounded text through', () => {
  it('strips them from a message and caps its length', () => {
    const evil = `Pay now ‮exe.txt‬\u0007\u0000 ${'x'.repeat(5000)}`;
    const out = describeError(new Error(evil));
    const bad = [...out].filter((ch) => {
      const c = ch.codePointAt(0)!;
      return c <= 0x1f || (c >= 0x202a && c <= 0x202e) || (c >= 0x2066 && c <= 0x2069);
    });
    expect(bad).toEqual([]);
    expect(out.length).toBeLessThanOrEqual(400);
  });
});
