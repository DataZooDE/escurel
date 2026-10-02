import * as assert from 'node:assert/strict';
import * as vscode from 'vscode';
import type { EscurelApi } from '../../../src/extension';

export const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));

/**
 * The three orders the seed holds. Ids are not purely numeric on purpose: the echo harness
 * re-serialises an instance's frontmatter, and an id like 4500123 does not survive the round
 * trip (the draft is refused with `frontmatter_required_key_missing`).
 *
 *  A page carries one open draft, so tests take one each. */
export const ORDERS = [
  'markdown/instances/customer-order__order-4500123.md',
  'markdown/instances/customer-order__order-4500124.md',
  'markdown/instances/customer-order__order-4500131.md',
];

export async function until<T>(
  f: () => Promise<T | undefined> | T | undefined,
  ms = 45_000,
  what = 'the condition',
): Promise<T> {
  const end = Date.now() + ms;
  for (;;) {
    const v = await f();
    if (v !== undefined) return v;
    if (Date.now() > end) throw new Error(`timed out waiting for ${what}`);
    await wait(300);
  }
}

/**
 * Activate the extension and sign it in with the test issuer's bearer.
 *
 * The gateway VERIFIES tokens (see runTests.ts for why that is the point), so the extension
 * needs one. It is handed through the API activation returns, not read from the environment by
 * the extension: nothing in a running install can be given a credential this way.
 */
export async function activate(): Promise<EscurelApi> {
  const ext = vscode.extensions.getExtension('datazoo.escurel')!;
  const api = (await ext.activate()) as EscurelApi;
  const bearer = process.env.ESCUREL_TEST_BEARER;
  assert.ok(bearer, 'the harness must provide ESCUREL_TEST_BEARER');
  api.services.auth.refresher.useStaticToken(bearer, process.env.ESCUREL_TEST_SUBJECT ?? 'alice');
  return api;
}

/** An order with no open draft, so a test owns that page's only draft slot. */
export async function freeOrder(api: EscurelApi): Promise<string> {
  const open = (await api.services.client.listDrafts()).filter((d) => d.status === 'open');
  const taken = new Set(open.map((d) => d.target_page_id));
  const free = ORDERS.find((p) => !taken.has(p));
  assert.ok(free, `every order already carries an open draft: ${[...taken].join(', ')}`);
  return free;
}

/**
 * The echo harness folds the OLDEST inbox event carrying a target instance, and a review run
 * leaves its event in the inbox until its draft is promoted. A draft left open therefore makes
 * the next test's run reach for that page, hit the one-draft-per-page rule and dead-letter —
 * which reads exactly like a runner that never started.
 */
export async function discardOpenDrafts(api: EscurelApi): Promise<void> {
  for (const d of await api.services.client.listDrafts()) {
    if (d.status === 'open') {
      await api.services.client
        .discardDraft({ draft_id: d.draft_id, reason: 'integration cleanup' })
        .catch(() => undefined);
    }
  }
  await settleInbox(api);
}

/**
 * Mark everything still in the inbox that has a target page as processed.
 *
 * The echo harness folds the OLDEST inbox event with a target page. A review run leaves its event
 * there until its draft is promoted, a plan run leaves its event there on purpose, and a
 * dead-lettered event stays there for a requeue, so one test's leftover silently becomes the event
 * every LATER test's run folds instead of its own (each of those runs is then a clean no-op and the
 * test times out waiting for a draft that never comes). Cleaning up after each test, rather than
 * hunting each test's own leftovers, is what keeps the suites independent.
 */
export async function settleInbox(api: EscurelApi): Promise<void> {
  const page = await api.services.client.listInbox({ limit: 100 });
  for (const e of page.events) {
    if (e.instance_page_id)
      await markProcessed(e.event_id, e.instance_page_id).catch(() => undefined);
  }
}

/**
 * Sign the extension in as the admin (same subject, admin role) for what only an admin may do.
 * Returns a function that signs it back in as the ordinary human.
 */
export function signInAsAdmin(api: EscurelApi): () => void {
  const admin = process.env.ESCUREL_TEST_ADMIN_BEARER;
  const human = process.env.ESCUREL_TEST_BEARER;
  assert.ok(admin && human, 'the harness must provide both bearers');
  const subject = process.env.ESCUREL_TEST_SUBJECT ?? 'alice';
  api.services.auth.refresher.useStaticToken(admin, subject);
  return () => api.services.auth.refresher.useStaticToken(human, subject);
}

/**
 * Mark an event processed and attach it to a page (`assign_event`), so it leaves the inbox.
 *
 * A run that ends `planned` leaves its event in the inbox on purpose, and the echo harness folds
 * the OLDEST inbox event that has a target page, so such an event would swallow every later test's
 * run. This is test hygiene, done with a raw call because the extension has no use for the tool.
 */
export async function markProcessed(eventId: string, pageId: string): Promise<void> {
  const url = process.env.ESCUREL_TEST_GATEWAY;
  const bearer = process.env.ESCUREL_TEST_BEARER;
  assert.ok(url && bearer, 'the harness must provide the gateway URL and a bearer');
  const res = await fetch(`${url.replace(/\/+$/, '')}/mcp`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', authorization: `Bearer ${bearer}` },
    body: JSON.stringify({
      jsonrpc: '2.0',
      id: 1,
      method: 'tools/call',
      params: { name: 'assign_event', arguments: { event_id: eventId, instance_page_id: pageId } },
    }),
  });
  const body = (await res.json()) as { error?: unknown };
  assert.equal(body.error, undefined, `assign_event: ${JSON.stringify(body.error)}`);
}
