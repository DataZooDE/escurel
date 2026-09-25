// A fake `/mcp` that answers from the recorded fixtures (test/unit/fixtures/*.json,
// captured against a real escurel-server). Routes by tool name, with a few
// argument-sensitive cases (cursor paging). Also plays the plain-HTTP auth
// refusals the gateway emits before any JSON-RPC envelope exists.
import { createServer, type Server } from 'node:http';
import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';

type Fixture = {
  tool: string;
  request: { params: { name: string; arguments: Record<string, unknown> } };
  status: number;
  response: unknown;
};

const fixtures: Record<string, Fixture> = {};
for (const f of readdirSync(join(__dirname, 'fixtures'))) {
  fixtures[f.replace(/\.json$/, '')] = JSON.parse(
    readFileSync(join(__dirname, 'fixtures', f), 'utf8'),
  );
}

export function fixture(name: string): Fixture {
  const f = fixtures[name];
  if (!f) throw new Error(`no fixture ${name}`);
  return f;
}

export interface MockGateway {
  url: string;
  /** Every tools/call the client made, in order. */
  calls: { name: string; arguments: Record<string, unknown>; authorization?: string }[];
  /** Force a plain-HTTP refusal on the next request (401 / 403 / 429 as the gateway emits them). */
  refuseNext: (status: number, body: unknown) => void;
  close: () => Promise<void>;
}

/** Pick the fixture that answers `tool` with `args`. */
function answer(tool: string, args: Record<string, unknown>): Fixture | undefined {
  switch (tool) {
    case 'list_instances':
      return args.cursor ? fixture('list_instances_page2') : fixture('list_instances_page1');
    case 'expand': {
      const id = String(args.page_id);
      if (id.includes('nope')) return fixture('expand_missing');
      // The recorded instance expand predates raw content: it plays the pre-#579 gateway.
      return id.startsWith('markdown/skills/') && args.raw
        ? fixture('expand_skill_raw')
        : fixture('expand_instance');
    }
    case 'resolve':
      if (typeof args.wikilink !== 'string') return fixture('invalid_params');
      return String(args.wikilink).includes('nobody')
        ? fixture('resolve_missing')
        : fixture('resolve_ok');
    case 'update_page':
      return args.base_sha256 ? fixture('update_page_conflict') : fixture('update_page_invalid');
    case 'capture_event':
      return args.label_skill === 'escurel:run-control'
        ? fixture('run_control_event_not_found')
        : fixture('capture_event');
    case 'mint_agent_token':
      return fixture('mint_unsupported');
    case 'list_events':
      return fixture('list_events_by_page');
    case 'list_lineage':
      return fixture('list_lineage_empty');
    case 'get_run_tool_calls':
      return fixture('get_run_tool_calls_empty');
    case 'diff_draft':
      return fixture('diff_draft_not_found');
    case 'promote_draft':
      return fixture('promote_draft_not_found');
    default:
      return Object.values(fixtures).find((f) => f.tool === tool);
  }
}

export async function startMockGateway(): Promise<MockGateway> {
  const calls: MockGateway['calls'] = [];
  let refusal: { status: number; body: unknown } | undefined;
  const server: Server = createServer((req, res) => {
    let raw = '';
    req.on('data', (c) => (raw += c));
    req.on('end', () => {
      if (req.method === 'GET') {
        res.writeHead(405).end();
        return;
      }
      if (refusal) {
        const r = refusal;
        refusal = undefined;
        res.writeHead(r.status, { 'content-type': 'application/json' }).end(JSON.stringify(r.body));
        return;
      }
      const msg = JSON.parse(raw);
      if (msg.method === 'initialize') {
        res.writeHead(200, { 'content-type': 'application/json' }).end(
          JSON.stringify({
            jsonrpc: '2.0',
            id: msg.id,
            result: {
              protocolVersion: msg.params.protocolVersion,
              capabilities: { tools: { listChanged: false } },
              serverInfo: { name: 'escurel', version: 'mock' },
            },
          }),
        );
        return;
      }
      if (String(msg.method).startsWith('notifications/')) {
        res.writeHead(202).end();
        return;
      }
      const { name, arguments: args = {} } = msg.params;
      calls.push({ name, arguments: args, authorization: req.headers.authorization });
      const f = answer(name, args);
      if (!f) {
        res.writeHead(200, { 'content-type': 'application/json' }).end(
          JSON.stringify({
            jsonrpc: '2.0',
            id: msg.id,
            error: { code: -32601, message: `mock: no fixture for ${name}` },
          }),
        );
        return;
      }
      const body = JSON.parse(JSON.stringify(f.response));
      body.id = msg.id;
      res.writeHead(f.status, { 'content-type': 'application/json' }).end(JSON.stringify(body));
    });
  });
  await new Promise<void>((r) => server.listen(0, '127.0.0.1', r));
  const addr = server.address() as { port: number };
  return {
    url: `http://127.0.0.1:${addr.port}`,
    calls,
    refuseNext: (status, body) => (refusal = { status, body }),
    close: () => new Promise((r) => server.close(() => r())),
  };
}
