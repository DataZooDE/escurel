import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { EscurelClient, EscurelError } from '../../src/client';
import { pageIdFromPath, pathForPage, readPageMarkdown } from '../../src/fs/read';
import { writeSkill } from '../../src/fs/write';
import { fixture, startMockGateway, type MockGateway } from './mockGateway';

let gw: MockGateway;
let client: EscurelClient;
beforeAll(async () => {
  gw = await startMockGateway();
  client = new EscurelClient({ gatewayUrl: gw.url, tokens: { get: async () => undefined } });
});
afterAll(async () => {
  await client.close();
  await gw.close();
});

describe('escurel: paths', () => {
  it('maps skill and instance page ids to escurel: paths and back, flat and nested layouts alike', () => {
    expect(pathForPage('markdown/skills/customer.md')).toBe('/skills/customer.md');
    expect(pathForPage('markdown/instances/customer__acme.md')).toBe(
      '/instances/customer__acme.md',
    );
    expect(pageIdFromPath('/skills/customer.md')).toEqual({
      pageId: 'markdown/skills/customer.md',
      kind: 'skill',
    });
    expect(pageIdFromPath('/instances/customer__acme.md')).toEqual({
      pageId: 'markdown/instances/customer__acme.md',
      kind: 'instance',
      skill: 'customer',
    });
    expect(pageIdFromPath('/instances/customer/acme.md')).toEqual({
      pageId: 'markdown/instances/customer/acme.md',
      kind: 'instance',
      skill: 'customer',
    });
    expect(pageIdFromPath('/instances/customer')).toEqual({
      kind: 'instances-skill',
      skill: 'customer',
    });
    expect(pageIdFromPath('/')).toEqual({ kind: 'root' });
    expect(pageIdFromPath('/skills')).toEqual({ kind: 'skills-root' });
    expect(pageIdFromPath('/nope/x.md')).toBeUndefined();
  });
});

describe('readPageMarkdown', () => {
  it('serves the stored markdown verbatim and its hash when the gateway returns content', async () => {
    const r = await readPageMarkdown(client, 'markdown/skills/customer.md');
    const raw = fixture('expand_skill_raw').response as {
      result: { structuredContent: { content: string; content_sha256: string } };
    };
    expect(r?.text).toBe(raw.result.structuredContent.content);
    expect(r?.sha256).toBe(raw.result.structuredContent.content_sha256);
    expect(r?.degraded).toBe(false);
    expect(gw.calls.at(-1)?.arguments.raw).toBe(true);
  });

  it('reassembles frontmatter + body, flagged degraded, when an older gateway sends no content', async () => {
    const r = await readPageMarkdown(client, 'markdown/instances/customer__acme.md');
    expect(r?.degraded).toBe(true);
    expect(r?.text.startsWith('---\n')).toBe(true);
    expect(r?.text).toContain('\n---\n');
    const inst = fixture('expand_instance').response as {
      result: { structuredContent: { body: string } };
    };
    expect(r?.text.endsWith(inst.result.structuredContent.body)).toBe(true);
  });

  it('a missing page reads as undefined', async () => {
    expect(await readPageMarkdown(client, 'markdown/instances/nope/x.md')).toBeUndefined();
  });
});

describe('writeSkill', () => {
  it('sends update_page with the base hash the page was read at', async () => {
    const err = await writeSkill(client, 'markdown/skills/customer.md', 'x', 'a'.repeat(64)).catch(
      (e) => e,
    );
    expect(gw.calls.at(-1)?.arguments.base_sha256).toBe('a'.repeat(64));
    expect(err).toBeInstanceOf(EscurelError);
    expect(err.kind).toBe('conflict');
    expect(typeof err.head_content).toBe('string');
  });
});
