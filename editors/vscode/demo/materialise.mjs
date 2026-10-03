// Materialises the demo's read-only SQL views (an admin step; a sql_view instance is not authored as
// markdown). Today one: `order-lines`, the order lines behind the supplier-risk analysis chart.
import { readFileSync } from 'node:fs';

const gw = JSON.parse(readFileSync(process.argv[2], 'utf8').split('\n')[0]);

async function call(name, args) {
  const res = await fetch(`${gw.gateway_url}/mcp`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', authorization: `Bearer ${gw.admin_bearer}` },
    body: JSON.stringify({
      jsonrpc: '2.0',
      id: 1,
      method: 'tools/call',
      params: { name, arguments: args },
    }),
  });
  const body = await res.json();
  if (body.error) throw new Error(`${name}: ${JSON.stringify(body.error)}`);
  return body.result.structuredContent;
}

await call('create_sql_instance', {
  skill: 'order-lines',
  id: 'all',
  overlay_body: "# Order lines\nRead-only mirror of the demo's order lines (a JSON extract).",
});
