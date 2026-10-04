// A tiny REAL REST service for the demo: the supplier-ratings API of a (fictional) procurement
// portal. It is a separate process on a real socket; escurel reaches it like any outside system.
//
//   GET   /ratings?limit=&after=   a page of suppliers, cursor = the last id of the previous page
//   GET   /ratings/:id             one supplier, with an ETag
//   PATCH /ratings/:id             change `rating` (A | B | C); needs If-Match and an Idempotency-Key,
//                                  applies each key once
//
// usage: node ratings-api.mjs [port]      prints {"port": N} when it is listening
import { createServer } from 'node:http';

const rows = new Map(
  [
    ['meier-guss', 'Meier-Guss GmbH', 'A', 96.2, 'DACH'],
    ['stahl-ag', 'Stahl AG', 'B', 88.4, 'DACH'],
    ['nordform', 'Nordform Metallbau A/S', 'B', 91.0, 'Nordics'],
    ['balkan-cast', 'Balkan Cast d.o.o.', 'C', 71.5, 'SEE'],
    ['iberica-forja', 'Ibérica Forja S.L.', 'A', 94.8, 'Iberia'],
    ['polska-odlew', 'Polska Odlew Sp. z o.o.', 'B', 86.9, 'CEE'],
  ].map(([id, name, rating, on_time_pct, region]) => [
    id,
    { id, name, rating, on_time_pct, region, updated: '2026-09-30' },
  ]),
);
let version = 1;
const appliedKeys = new Set();
const etag = () => `"v${version}"`;

function send(res, status, body, headers = {}) {
  const text = body === undefined ? '' : JSON.stringify(body);
  res.writeHead(status, { 'content-type': 'application/json', ...headers });
  res.end(text);
}

const server = createServer((req, res) => {
  const url = new URL(req.url, 'http://x');
  const m = url.pathname.match(/^\/ratings(?:\/([^/]+))?$/);
  if (!m) return send(res, 404, { error: 'not found' });
  const id = m[1] ? decodeURIComponent(m[1]) : undefined;

  if (req.method === 'GET' && !id) {
    const limit = Math.min(Number(url.searchParams.get('limit')) || 50, 200);
    const after = url.searchParams.get('after') ?? '';
    const all = [...rows.values()].sort((a, b) => a.id.localeCompare(b.id));
    const page = all.filter((r) => !after || r.id > after).slice(0, limit);
    const last = page.at(-1);
    const more = last && all.some((r) => r.id > last.id);
    return send(res, 200, { data: page, paging: { next: more ? last.id : null } });
  }
  if (req.method === 'GET' && id) {
    const r = rows.get(id);
    return r ? send(res, 200, r, { etag: etag() }) : send(res, 404, { error: 'no such supplier' });
  }
  if (req.method === 'PATCH' && id) {
    let raw = '';
    req.on('data', (c) => (raw += c));
    req.on('end', () => {
      const r = rows.get(id);
      if (!r) return send(res, 404, { error: 'no such supplier' });
      const key = req.headers['idempotency-key'];
      if (!key) return send(res, 400, { error: 'Idempotency-Key is required' });
      if (appliedKeys.has(key)) return send(res, 200, { ok: true, replayed: true });
      const match = req.headers['if-match'];
      if (match && match !== etag()) return send(res, 412, { error: 'the supplier changed' });
      let body;
      try {
        body = JSON.parse(raw);
      } catch {
        return send(res, 400, { error: 'invalid JSON' });
      }
      if (body.rating !== undefined && !['A', 'B', 'C'].includes(body.rating))
        return send(res, 400, { error: 'rating must be A, B or C' });
      Object.assign(r, body.rating === undefined ? {} : { rating: body.rating }, {
        updated: new Date().toISOString().slice(0, 10),
      });
      appliedKeys.add(key);
      version += 1;
      return send(res, 200, { ok: true });
    });
    return;
  }
  return send(res, 405, { error: 'method not allowed' });
});

server.listen(Number(process.argv[2]) || 0, '127.0.0.1', () => {
  console.log(JSON.stringify({ port: server.address().port }));
});
