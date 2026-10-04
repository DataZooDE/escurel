import { describe, expect, it } from 'vitest';
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';

// A CSP nonce (or any id a webview trusts) must come from the platform CSPRNG. The skill page once
// built its nonce with Math.random; this keeps every html builder on newNonce().
function files(dir: string, out: string[] = []): string[] {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) files(p, out);
    else if (p.endsWith('.ts')) out.push(p);
  }
  return out;
}

describe('no Math.random in the extension host or the webviews', () => {
  it('finds none in src/ or webview/', () => {
    const root = join(__dirname, '..', '..');
    const hits = [...files(join(root, 'src')), ...files(join(root, 'webview'))].filter((f) =>
      /Math\s*\.\s*random\s*\(/.test(readFileSync(f, 'utf8').replace(/\/\/.*$|\/\*[\s\S]*?\*\//gm, '')),
    );
    expect(hits).toEqual([]);
  });
});
