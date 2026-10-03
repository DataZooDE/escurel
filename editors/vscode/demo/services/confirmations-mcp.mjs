// A tiny REAL MCP server for the demo: delivery confirmations of purchase orders, over the spec's
// streamable-HTTP transport (initialize, a session id, notifications/initialized, JSON or SSE).
//
//   tools: listConfirmations {after?, limit?}   getConfirmation {id}
//          updateConfirmation {id, status, idempotency_key?}   (status: open | confirmed | moved)
//
// usage: node confirmations-mcp.mjs [port]      prints {"port": N} when it is listening
import { createServer } from 'node:http';
import { randomUUID } from 'node:crypto';

const PROTOCOL = '2025-06-18';
const rows = new Map(
  [
    ['PO-4500087412-10', 'PO 4500087412', 10, 'GH-4711 Gearbox housing', 240, 240, '2026-10-12', 'confirmed'],
    ['PO-4500087412-20', 'PO 4500087412', 20, 'TH-0815 Tool holder', 80, 0, '2026-10-26', 'moved'],
    ['PO-4500087433-10', 'PO 4500087433', 10, 'GH-4711 Gearbox housing', 200, 120, '2026-10-19', 'open'],
    ['PO-4500087501-10', 'PO 4500087501', 10, 'SH-2290 Shaft, hardened', 60, 60, '2026-10-15', 'confirmed'],
  ].map(([id, po, line, material, qty_ordered, qty_confirmed, confirmed_date, status]) => [
    id,
    { id, po, line, material, qty_ordered, qty_confirmed, confirmed_date, status },
  ]),
);
const sessions = new Set();
const applied = new Set();

const rpc = (id, result) => ({ jsonrpc: '2.0', id, result });
const rpcError = (id, code, message) => ({ jsonrpc: '2.0', id, error: { code, message } });
const asText = (v) => ({ content: [{ type: 'text', text: JSON.stringify(v) }], isError: false });

function callTool(name, args) {
  if (name === 'listConfirmations') {
    const limit = Math.min(Number(args.limit) || 50, 200);
    const all = [...rows.values()].sort((a, b) => a.id.localeCompare(b.id));
    const page = all.filter((r) => !args.after || r.id > args.after).slice(0, limit);
    const last = page.at(-1);
    const next = last && all.some((r) => r.id > last.id) ? last.id : null;
    return asText({ confirmations: page, next });
  }
  if (name === 'getConfirmation') {
    const r = rows.get(args.id);
    return { structuredContent: r ?? {}, content: [], isError: false };
  }
  if (name === 'updateConfirmation') {
    const r = rows.get(args.id);
    if (!r) return { content: [{ type: 'text', text: 'no such confirmation' }], isError: true };
    if (!['open', 'confirmed', 'moved'].includes(args.status))
      return { content: [{ type: 'text', text: 'status must be open, confirmed or moved' }], isError: true };
    const key = args.idempotency_key;
    if (!key || !applied.has(key)) {
      r.status = args.status;
      if (key) applied.add(key);
    }
    return { structuredContent: { ok: true }, content: [], isError: false };
  }
  return null;
}

const tools = [
  { name: 'listConfirmations', inputSchema: { type: 'object', properties: { after: { type: 'string' }, limit: { type: 'integer' } } } },
  { name: 'getConfirmation', inputSchema: { type: 'object', properties: { id: { type: 'string' } } } },
  { name: 'updateConfirmation', inputSchema: { type: 'object', properties: { id: { type: 'string' }, status: { type: 'string' }, idempotency_key: { type: 'string' } } } },
];

const server = createServer((req, res) => {
  if (req.url !== '/mcp') { res.writeHead(404).end(); return; }
  if (req.method !== 'POST') { res.writeHead(405, { allow: 'POST' }).end(); return; }
  let raw = '';
  req.on('data', (c) => (raw += c));
  req.on('end', () => {
    let msg;
    try { msg = JSON.parse(raw); } catch { res.writeHead(400).end(); return; }
    const json = (status, body, headers = {}) => {
      res.writeHead(status, { 'content-type': 'application/json', ...headers });
      res.end(JSON.stringify(body));
    };
    if (msg.method === 'initialize') {
      const sid = randomUUID();
      sessions.add(sid);
      return json(200, rpc(msg.id, {
        protocolVersion: PROTOCOL,
        capabilities: { tools: { listChanged: false } },
        serverInfo: { name: 'delivery-confirmations', version: '1' },
      }), { 'mcp-session-id': sid });
    }
    const sid = req.headers['mcp-session-id'];
    if (!sid) { res.writeHead(400).end('missing Mcp-Session-Id'); return; }
    if (!sessions.has(sid)) { res.writeHead(404).end('unknown session'); return; }
    if (msg.method === 'notifications/initialized') { res.writeHead(202).end(); return; }
    if (msg.method === 'tools/list') return json(200, rpc(msg.id, { tools }));
    if (msg.method === 'tools/call') {
      const out = callTool(msg.params?.name, msg.params?.arguments ?? {});
      if (!out) return json(200, rpcError(msg.id, -32601, 'unknown tool'));
      // Listings arrive as server-sent events, as many MCP servers send them.
      if (msg.params.name === 'listConfirmations') {
        res.writeHead(200, { 'content-type': 'text/event-stream' });
        res.end(`event: message\ndata: ${JSON.stringify(rpc(msg.id, out))}\n\n`);
        return;
      }
      return json(200, rpc(msg.id, out));
    }
    return json(200, rpcError(msg.id, -32601, `unknown method ${msg.method}`));
  });
});

server.listen(Number(process.argv[2]) || 0, '127.0.0.1', () => {
  console.log(JSON.stringify({ port: server.address().port }));
});
