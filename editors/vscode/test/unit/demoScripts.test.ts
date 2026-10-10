// The two things demo/run.sh used to get wrong (wiping a user-set ESCUREL_DEMO_HOME, running on a stale libduckdb)
// live in demo/lib.sh; the desktop bootstrap's opening logic is `openStart`.
import { spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

const demo = join(__dirname, '../../demo');
const tmp = () => mkdtempSync(join(tmpdir(), 'escurel-demo-test-'));

function sh(
  script: string,
  env: Record<string, string> = {},
): { out: string; status: number | null } {
  const r = spawnSync('bash', ['-c', `. "${demo}/lib.sh"; ${script}`], {
    env: { PATH: process.env.PATH ?? '', HOME: '/home/someone', ...env },
    encoding: 'utf8',
  });
  return { out: r.stdout.trim(), status: r.status };
}

describe('demo_home_resettable', () => {
  const ok = (d: string) => sh(`demo_home_resettable "${d}"`).status === 0;

  it('accepts an absent, an empty, a marked and a demo-made directory', () => {
    const root = tmp();
    expect(ok(join(root, 'absent'))).toBe(true);
    const empty = join(root, 'empty');
    mkdirSync(empty);
    expect(ok(empty)).toBe(true);
    const marked = join(root, 'marked');
    mkdirSync(marked);
    writeFileSync(join(marked, '.escurel-demo-home'), '');
    writeFileSync(join(marked, 'precious'), 'x');
    expect(ok(marked)).toBe(true);
    const old = join(root, 'old');
    mkdirSync(join(old, 'profile'), { recursive: true });
    writeFileSync(join(old, 'gateway.json'), '{}');
    expect(ok(old)).toBe(true);
  });

  it('refuses a directory that is not a demo home, a file, "/" and $HOME', () => {
    const root = tmp();
    const other = join(root, 'projects');
    mkdirSync(other);
    writeFileSync(join(other, 'work.txt'), 'mine');
    expect(ok(other)).toBe(false);
    const file = join(root, 'a-file');
    writeFileSync(file, 'x');
    expect(ok(file)).toBe(false);
    expect(ok('/')).toBe(false);
    expect(sh('demo_home_resettable "$HOME"').status).not.toBe(0);
    expect(sh('demo_home_resettable ""').status).not.toBe(0);
  });
});

describe('libduckdb_dir', () => {
  // A fake repo: Cargo.lock pinning libduckdb-sys 1.10506.0 (DuckDB 1.5.6) and a target/ with downloads.
  function repo(versions: string[]): { env: Record<string, string>; dir: string } {
    const root = tmp();
    writeFileSync(
      join(root, 'Cargo.lock'),
      '[[package]]\nname = "libduckdb-sys"\nversion = "1.10506.0"\n',
    );
    mkdirSync(join(root, 'target/release'), { recursive: true });
    for (const v of versions) {
      mkdirSync(join(root, 'target/duckdb-download/linux-amd64', v), { recursive: true });
      writeFileSync(join(root, 'target/duckdb-download/linux-amd64', v, 'libduckdb.so'), '');
    }
    const env = {
      HERE: demo,
      REPO: root,
      GATEWAY_BIN: join(root, 'target/release/escurel-test-gateway'),
    };
    return { env, dir: root };
  }

  it("returns the pinned version's copy", () => {
    const { env, dir } = repo(['1.5.5', '1.5.6']);
    expect(resolve(sh('libduckdb_dir', env).out)).toBe(
      join(dir, 'target/duckdb-download/linux-amd64/1.5.6'),
    );
  });

  it('returns nothing when the pinned copy is missing, instead of the newest stale one', () => {
    const { env } = repo(['1.5.5']);
    expect(sh('libduckdb_dir', env).out).toBe('');
  });

  it('honours ESCUREL_DEMO_LIBDUCKDB_DIR', () => {
    const { env } = repo([]);
    expect(sh('libduckdb_dir', { ...env, ESCUREL_DEMO_LIBDUCKDB_DIR: '/opt/duck' }).out).toBe(
      '/opt/duck',
    );
  });
});

describe('the desktop bootstrap opens ESCUREL_DEMO_OPEN_PAGE in both looks', () => {
  type Mod = { _load: (req: string, ...rest: unknown[]) => unknown };
  async function run(env: Record<string, string | undefined>, story?: object): Promise<string[]> {
    const calls: string[] = [];
    const fakeVscode = {
      commands: {
        executeCommand: async (c: string, ...a: unknown[]) =>
          void calls.push([c, ...a.map((x) => JSON.stringify(x))].join(' ')),
      },
    };
    const Module = createRequire(__filename)('node:module') as Mod;
    const orig = Module._load;
    Module._load = (req, ...rest) =>
      req === 'vscode' ? fakeVscode : orig.call(Module, req, ...rest);
    try {
      const bootstrap = createRequire(__filename)(join(demo, 'bootstrap/extension.js')) as {
        openStart: (v: unknown, e: unknown, fs: unknown) => Promise<void>;
      };
      const fs = { readFileSync: () => JSON.stringify(story ?? {}) };
      await bootstrap.openStart(fakeVscode, env, fs);
    } finally {
      Module._load = orig;
    }
    return calls;
  }

  it('calm look: focus mode with the overview, then the page', async () => {
    const calls = await run({ ESCUREL_DEMO_OPEN_PAGE: 'markdown/instances/x/y.md' });
    expect(calls).toEqual([
      'escurel.focusMode.enter {"silent":true,"overview":true}',
      'escurel.openInstance "markdown/instances/x/y.md"',
    ]);
  });

  it('classic look: the story thread, then the page', async () => {
    const calls = await run(
      {
        ESCUREL_DEMO_FOCUS: '0',
        ESCUREL_DEMO_STORY: '/story.json',
        ESCUREL_DEMO_OPEN_PAGE: 'p.md',
      },
      { rootA: 'EV1' },
    );
    expect(calls).toEqual(['escurel.openThread "EV1"', 'escurel.openInstance "p.md"']);
  });

  it('opens nothing extra when there is no page', async () => {
    expect(await run({})).toEqual(['escurel.focusMode.enter {"silent":true,"overview":true}']);
  });
});
