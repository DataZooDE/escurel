import { StreamableHTTPClientTransport } from '@modelcontextprotocol/sdk/client/streamableHttp.js';
import type { TokenSource } from '../auth/tokenSource';
import { EscurelError } from './errors';

/**
 * The SDK's streamable-HTTP transport pointed at `<gateway>/mcp`. The
 * gateway is POST-only (its GET answers 405, which the SDK treats as "no
 * SSE stream" by spec), so this is plain JSON-RPC per call in practice.
 *
 * The bearer is injected per request from the shared token source, so a
 * refresh needs no new transport; a plain-HTTP refusal (401/403/429, no
 * envelope) is turned into an `EscurelError` before the SDK sees it.
 */
export function createTransport(
  gatewayUrl: string,
  tokens: TokenSource,
): StreamableHTTPClientTransport {
  const fetchWithAuth = async (input: string | URL, init?: RequestInit): Promise<Response> => {
    const headers = new Headers(init?.headers);
    const token = await tokens.get();
    if (token) headers.set('authorization', `Bearer ${token}`);
    const res = await fetch(input, { ...init, headers });
    if (res.status === 401 || res.status === 403 || res.status === 429) {
      let body: { error?: string; message?: string } | undefined;
      try {
        body = (await res.json()) as typeof body;
      } catch {
        body = undefined;
      }
      throw EscurelError.fromHttp(res.status, body);
    }
    return res;
  };
  return new StreamableHTTPClientTransport(new URL('/mcp', gatewayUrl.replace(/\/+$/, '') + '/'), {
    fetch: fetchWithAuth as unknown as typeof fetch,
  });
}
