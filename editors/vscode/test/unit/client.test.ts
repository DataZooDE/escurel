import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { EscurelClient } from '../../src/client';
import { EscurelError } from '../../src/client/errors';
import { fixture, startMockGateway, type MockGateway } from './mockGateway';

let gw: MockGateway;
let client: EscurelClient;
let token: string | undefined = 'tok-1';

beforeAll(async () => {
  gw = await startMockGateway();
  client = new EscurelClient({ gatewayUrl: gw.url, tokens: { get: async () => token } });
});
afterAll(async () => {
  await client.close();
  await gw.close();
});

describe('typed wrapper', () => {
  it('lists skills with the wire shape and sends the bearer', async () => {
    const skills = await client.listSkills();
    expect(skills.map((s) => s.id)).toContain('customer');
    const customer = skills.find((s) => s.id === 'customer')!;
    expect(customer.layer).toBe('overlay');
    expect(gw.calls.at(-1)?.authorization).toBe('Bearer tok-1');
  });

  it('sends no Authorization header when the token source has none (dev gateway)', async () => {
    token = undefined;
    await client.listSkills();
    expect(gw.calls.at(-1)?.authorization).toBeUndefined();
    token = 'tok-1';
  });

  it('pages list_instances until next_cursor is null', async () => {
    const pages: number[] = [];
    for await (const page of client.listInstances({ skill_id: 'customer', limit: 2 }))
      pages.push(page.instances.length);
    expect(pages.length).toBe(2);
    const second = gw.calls.filter((c) => c.name === 'list_instances')[1];
    const page1 = fixture('list_instances_page1').response as {
      result: { structuredContent: { next_cursor: string } };
    };
    expect(second?.arguments.cursor).toBe(page1.result.structuredContent.next_cursor);
  });

  it('expand returns the stored markdown and its hash with raw', async () => {
    const e = await client.expand({ page_id: 'markdown/skills/customer.md', raw: true });
    expect(typeof e.content).toBe('string');
    expect(e.content_sha256).toMatch(/^[0-9a-f]{64}$/);
    const missing = await client.expand({ page_id: 'markdown/instances/nope/x.md' });
    expect(missing.page).toBeNull();
  });

  it('validate returns issues without throwing (ok:false is not a refusal there)', async () => {
    const v = await client.validate({ content: 'x', as_page_id: 'markdown/skills/x.md' });
    expect(v.ok).toBe(false);
    expect(v.issues.map((i) => i.code)).toContain('field_render_unknown');
  });

  it('update_page with a stale base_sha256 is a conflict carrying the head', async () => {
    const err = await client
      .updatePage({ page_id: 'p', content: 'c', base_sha256: 'a'.repeat(64) })
      .catch((e) => e);
    expect(err).toBeInstanceOf(EscurelError);
    expect(err.kind).toBe('conflict');
    expect(err.head_sha256).toMatch(/^[0-9a-f]{64}$/);
    expect(typeof err.head_content).toBe('string');
  });

  it('a rejected write is a refusal with its issues', async () => {
    const err = await client
      .updatePage({ page_id: 'p', content: 'no frontmatter\n' })
      .catch((e) => e);
    expect(err.kind).toBe('refused');
    expect(err.issues[0].code).toBe('frontmatter_parse');
  });

  it('JSON-RPC errors map by data.code: event_not_found, unsupported, invalid params', async () => {
    const nf = await client
      .captureEvent({
        label_skill: 'escurel:run-control',
        mime: 'application/json',
        body: '{"action":"cancel","run_id":"nope"}',
      })
      .catch((e) => e);
    expect(nf.kind).toBe('event_not_found');
    const un = await client.mintAgentToken({ skill: 'customer' }).catch((e) => e);
    expect(un.kind).toBe('unsupported');
    const bad = await client.resolve({ wikilink: 42 as unknown as string }).catch((e) => e);
    expect(bad.kind).toBe('invalid_params');
  });

  it('a not_found draft reads as absence: diff_draft and promote_draft', async () => {
    const d = await client.diffDraft({ draft_id: 'nope' }).catch((e) => e);
    expect(d.kind).toBe('not_found');
    const p = await client.promoteDraft({ draft_id: 'nope' }).catch((e) => e);
    expect(p.kind).toBe('not_found');
  });

  it('plain-HTTP refusals before any envelope: unauthorized, forbidden, session cap', async () => {
    gw.refuseNext(401, { error: 'unauthorized', message: 'token rejected: ExpiredSignature' });
    const u = await client.listSkills().catch((e) => e);
    expect(u.kind).toBe('unauthorized');
    gw.refuseNext(403, { error: 'forbidden', message: 'wrong tenant' });
    const f = await client.listSkills().catch((e) => e);
    expect(f.kind).toBe('forbidden');
    gw.refuseNext(429, { error: 'session_cap_reached', message: 'cap' });
    const c = await client.listSkills().catch((e) => e);
    expect(c.kind).toBe('session_cap_reached');
    expect(c.retryable).toBe(true);
  });

  it('already_decided and base_moved shapes are typed from their payloads', () => {
    // Hand-written from crates/escurel-server/src/mcp/tools_drafts.rs — the
    // recorded corpus holds no decided draft.
    const decided = EscurelError.fromPayload('promote_draft', {
      ok: false,
      issues: [
        { severity: 'error', code: 'already_decided', location: 'draft_id', message: 'decided' },
      ],
      draft: { draft_id: 'd1', status: 'promoted' },
    });
    expect(decided.kind).toBe('already_decided');
    expect(decided.draft?.status).toBe('promoted');
    const forbidden = EscurelError.fromPayload('update_page', {
      ok: false,
      issues: [{ severity: 'error', code: 'forbidden', location: 'frontmatter', message: 'no' }],
    });
    expect(forbidden.kind).toBe('forbidden');
  });
});
