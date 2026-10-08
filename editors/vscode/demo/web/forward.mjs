// The S2D stack's front door: a small reverse proxy, 0.0.0.0:<port> -> the gateway's loopback port (read from
// gateway.json, which the gateway writes once it is up; the test gateway binds 127.0.0.1 on a random port).
//
// It also SIGNS THE CALLER IN: it removes any Authorization header and adds the demo user's bearer (read from
// bearer.json, which the gateway keeps fresh). So the workbench's extension runs in its plain "no token" mode and
// never holds a credential: no token, no admin bearer and no signing key ever enter the workbench container. The
// listener is reachable only on the compose network (nothing publishes it), and the bearer is the USER's, not the admin's.
// HTTP (incl. streaming responses) and WebSocket upgrades both go through.
import http from 'node:http';
import net from 'node:net';
import { readFileSync } from 'node:fs';

const listen = Number(process.argv[2] ?? 8080);
const gatewayFile = process.argv[3] ?? '/demo/state/gateway.json';
const bearerFile = process.argv[4] ?? '/demo/state/bearer.json';

const first = (file) => JSON.parse(readFileSync(file, 'utf8').split('\n')[0]);
const gatewayPort = () => {
  try {
    return Number(new URL(first(gatewayFile).gateway_url).port) || null;
  } catch {
    return null;
  }
};
const bearer = () => {
  try {
    return first(bearerFile).bearer || null;
  } catch {
    return null;
  }
};

const server = http.createServer((req, res) => {
  const port = gatewayPort();
  if (!port) return res.writeHead(503, { 'content-type': 'text/plain' }).end('the gateway is not up yet');
  const headers = { ...req.headers, host: `127.0.0.1:${port}` };
  delete headers.authorization;
  const token = bearer();
  if (token) headers.authorization = `Bearer ${token}`;
  const upstream = http.request({ host: '127.0.0.1', port, method: req.method, path: req.url, headers }, (up) => {
    res.writeHead(up.statusCode ?? 502, up.headers);
    up.pipe(res);
  });
  upstream.on('error', () => {
    if (!res.headersSent) res.writeHead(502, { 'content-type': 'text/plain' });
    res.end('the gateway did not answer');
  });
  req.pipe(upstream);
});

server.on('upgrade', (req, socket, head) => {
  const port = gatewayPort();
  if (!port) return socket.destroy();
  const upstream = net.connect(port, '127.0.0.1', () => {
    const token = bearer();
    const lines = [`${req.method} ${req.url} HTTP/1.1`];
    for (let i = 0; i < req.rawHeaders.length; i += 2) {
      const name = req.rawHeaders[i];
      if (name.toLowerCase() === 'authorization') continue;
      lines.push(`${name}: ${name.toLowerCase() === 'host' ? `127.0.0.1:${port}` : req.rawHeaders[i + 1]}`);
    }
    if (token) lines.push(`Authorization: Bearer ${token}`);
    upstream.write(`${lines.join('\r\n')}\r\n\r\n`);
    if (head?.length) upstream.write(head);
    socket.pipe(upstream);
    upstream.pipe(socket);
  });
  const end = () => {
    socket.destroy();
    upstream.destroy();
  };
  socket.on('error', end);
  upstream.on('error', end);
  socket.on('close', end);
  upstream.on('close', end);
});

server.listen(listen, '0.0.0.0', () => console.log(`proxy :${listen} -> gateway (signed in as the demo user)`));
