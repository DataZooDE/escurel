// Forwards 0.0.0.0:<port> to the gateway's loopback port (read from gateway.json, which the gateway writes
// once it is up). The test gateway binds 127.0.0.1 on a random port; this is what makes it `escurel:8080`
// for the workbench. Plain TCP, so WebSocket upgrades pass through untouched.
import net from 'node:net';
import { readFileSync } from 'node:fs';

const listen = Number(process.argv[2] ?? 8080);
const stateFile = process.argv[3] ?? '/demo/state/gateway.json';
const port = () => {
  try {
    return Number(new URL(JSON.parse(readFileSync(stateFile, 'utf8').split('\n')[0]).gateway_url).port) || null;
  } catch {
    return null;
  }
};
net
  .createServer((client) => {
    const p = port();
    if (!p) return client.destroy();
    const upstream = net.connect(p, '127.0.0.1');
    client.pipe(upstream);
    upstream.pipe(client);
    const end = () => {
      client.destroy();
      upstream.destroy();
    };
    client.on('error', end);
    upstream.on('error', end);
    client.on('close', end);
    upstream.on('close', end);
  })
  .listen(listen, '0.0.0.0', () => console.log(`forwarding :${listen} -> gateway (${stateFile})`));
