#!/usr/bin/env node
// Captures the wire-contract golden files from a REAL gateway (and a real runner).
//
//   ESCUREL_BIN_DIR=target/release node scripts/refresh-golden.mjs
//
// Writes crates/escurel-types/tests/golden/*.json. The Rust (`golden_contract`), TypeScript (vitest) and
// Dart tests all decode the same files, so a wire change breaks all three at once instead of drifting
// silently between the hand-copied types. Commit the result with the wire change that caused it.
//
// Each file is a tool call's `structuredContent`, except `*_result.json`, which is the whole result
// (isError + content), because those pin what a refusal looks like. Tokens are never written.
import { spawn } from 'node:child_process';
import { cpSync, mkdirSync, mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const bin = resolve(process.env.ESCUREL_BIN_DIR ?? join(root, 'target/release'));
const outDir = join(root, 'crates/escurel-types/tests/golden');
const seedSrc = join(root, 'editors/vscode/test/integration/seed');
// The seed plus one NESTED instance (`instances/<skill>/<id>.md`): the autonomy gate keys on that layout.
const seed = join(mkdtempSync(join(tmpdir(), 'golden-seed-')), 'seed');
cpSync(seedSrc, seed, { recursive: true });
mkdirSync(join(seed, 'instances/customer-order'), { recursive: true });
writeFileSync(
  join(seed, 'instances/customer-order/golden-1.md'),
  '---\nkind: instance\nskill: customer-order\nid: golden-1\n---\n# golden-1\n\nBaseline.\n',
);
mkdirSync(outDir, { recursive: true });

const children = [];
const stop = () => children.forEach((c) => c.kill('SIGTERM'));
process.on('exit', stop);

function startGateway() {
  const proc = spawn(join(bin, 'escurel-test-gateway'), ['--tenant', 'golden', '--seed', seed, '--subject', 'alice'], {
    stdio: ['ignore', 'pipe', 'inherit'],
  });
  children.push(proc);
  return new Promise((ok, fail) => {
    let buf = '';
    proc.stdout.on('data', (d) => {
      buf += d;
      const nl = buf.indexOf('\n');
      if (nl >= 0) ok(JSON.parse(buf.slice(0, nl)));
    });
    proc.on('exit', (c) => fail(new Error(`gateway exited ${c}`)));
  });
}

let id = 0;
async function call(info, bearer, name, args) {
  const res = await fetch(`${info.gateway_url}/mcp`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', accept: 'application/json', authorization: `Bearer ${bearer}` },
    body: JSON.stringify({ jsonrpc: '2.0', id: ++id, method: 'tools/call', params: { name, arguments: args } }),
  });
  const body = await res.json();
  if (body.error) throw new Error(`${name}: ${JSON.stringify(body.error)}`);
  return body.result;
}

const save = (name, value) => {
  writeFileSync(join(outDir, `${name}.json`), `${JSON.stringify(value, null, 2)}\n`);
  console.log(`wrote ${name}.json`);
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const info = await startGateway();
const human = info.bearer;
const page = 'markdown/instances/customer-order__order-4500123.md';

save('list_skills', (await call(info, human, 'list_skills', {})).structuredContent);
save('expand_instance', (await call(info, human, 'expand', { page_id: page })).structuredContent);
save('list_instances', (await call(info, human, 'list_instances', { skill_id: 'customer-order', limit: 2 })).structuredContent);

// A refusal: the whole result, isError and all.
save(
  'refusal_result',
  await call(info, human, 'update_page', { page_id: 'markdown/skills/broken.md', content: 'no frontmatter\n' }),
);
save('validate_result', await call(info, human, 'validate', { content: 'x', as_page_id: 'markdown/skills/x.md' }));

// held_for_review: a machine run token writes a page of a `review` skill.
const minted = (await call(info, human, 'mint_agent_token', {
  skill: 'customer-order',
  target_page_id: 'markdown/instances/customer-order/golden-1.md',
})).structuredContent;
const nested = 'markdown/instances/customer-order/golden-1.md';
const exp = await call(info, human, 'expand', { page_id: nested, raw: true });
const held = await call(info, minted.token, 'update_page', {
  page_id: nested,
  content: `${exp.structuredContent.content}\n\nGolden probe.\n`,
  base_sha256: exp.structuredContent.content_sha256,
});
save('update_page_held', held.structuredContent);

// Run events: a real runner (echo harness) folds a captured event.
const runnerDir = mkdtempSync(join(tmpdir(), 'golden-runner-'));
const runner = spawn(join(bin, 'escurel-runner'), [], {
  stdio: ['ignore', 'ignore', 'inherit'],
  env: {
    ...process.env,
    ESCUREL_RUNNER_GATEWAY_URL: info.gateway_url,
    ESCUREL_RUNNER_TENANT: info.tenant,
    ESCUREL_RUNNER_AUTH_ISSUER: info.issuer_url,
    ESCUREL_RUNNER_AUTH_KID: info.kid,
    ESCUREL_RUNNER_AUTH_SIGNING_KEY: info.signing_key,
    ESCUREL_RUNNER_HARNESS: 'echo',
    ESCUREL_RUNNER_LISTEN: '127.0.0.1:0',
    ESCUREL_RUNNER_LEDGER_PATH: join(runnerDir, 'ledger.duckdb'),
    ESCUREL_RUNNER_POLL_INTERVAL: '250ms',
    ESCUREL_RUNNER_TOKEN: '',
  },
});
children.push(runner);
const captured = (await call(info, human, 'capture_event', {
  label_skill: 'customer-order',
  mime: 'text/plain',
  source: 'golden',
  title: 'Golden run',
  body: 'Golden run.',
  instance_page_id: 'markdown/instances/customer-order__order-4500124.md',
})).structuredContent;
let runEvents;
for (let i = 0; i < 120; i += 1) {
  const r = (await call(info, human, 'list_events', {
    label_skill: 'escurel:run',
    include_system: true,
    root_event_id: captured.event_id,
  })).structuredContent;
  if (r.events.some((e) => e.title === 'run-finished')) {
    runEvents = r;
    break;
  }
  await sleep(500);
}
if (!runEvents) throw new Error('the runner never finished the run');
save('events_run', runEvents);
save('inbox_event', { event: (await call(info, human, 'list_events', { event_id: captured.event_id })).structuredContent.events[0] });
stop();
