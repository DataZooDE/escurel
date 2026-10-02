import { describe, expect, it } from 'vitest';
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { join } from 'node:path';

/**
 * The manifest and the code must agree, in both directions.
 *
 * A command declared and never registered fails when the user runs it from the
 * palette; a view declared with no provider renders VS Code's own "no data
 * provider registered" error in the sidebar. Neither shows up in a type check, a
 * unit test of any module, or a build — the same shape of drift as a tool schema
 * that under-declares its handler, which nothing was watching either.
 */
function sources(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) return sources(path);
    return name.endsWith('.ts') ? [readFileSync(path, 'utf8')] : [];
  });
}

const manifest = JSON.parse(readFileSync(join(__dirname, '..', '..', 'package.json'), 'utf8')) as {
  contributes: {
    commands: { command: string }[];
    views: Record<string, { id: string }[]>;
    configuration: { properties: Record<string, unknown> };
    menus: Record<string, { command: string; when?: string; group?: string }[]>;
  };
  capabilities: { untrustedWorkspaces: { restrictedConfigurations: string[] } };
};
const code = sources(join(__dirname, '..', '..', 'src')).join('\n');

describe('the manifest and the code agree', () => {
  it('declares only known commands in menus and wires the Runner controls', () => {
    const declared = new Set(manifest.contributes.commands.map((c) => c.command));
    const entries = Object.values(manifest.contributes.menus).flat();
    expect(entries.filter((entry) => !declared.has(entry.command))).toEqual([]);
    const runner = entries.filter((entry) => entry.when?.includes('escurel.runner'));
    expect(runner.map((entry) => entry.command).sort()).toEqual(
      [
        'escurel.cancelRun',
        'escurel.pauseDispatch',
        'escurel.requeue',
        'escurel.resumeDispatch',
        'escurel.resumeDispatch',
        'escurel.retryRun',
      ].sort(),
    );
  });

  it('every declared command is registered', () => {
    const declared = manifest.contributes.commands.map((c) => c.command);
    const missing = declared.filter((c) => !code.includes(`'${c}'`));
    expect(missing).toEqual([]);
  });

  it('every declared view has a provider', () => {
    const ids = Object.values(manifest.contributes.views).flatMap((vs) => vs.map((v) => v.id));
    const missing = ids.filter((id) => !code.includes(`'${id}'`));
    expect(missing).toEqual([]);
  });

  it('every setting is listed in restrictedConfigurations', () => {
    // A setting absent from the list is silently ignored in a Restricted Mode
    // window, which is how a fresh folder opens. That cost a release once.
    const declared = Object.keys(manifest.contributes.configuration.properties);
    const allowed = new Set(manifest.capabilities.untrustedWorkspaces.restrictedConfigurations);
    expect(declared.filter((k) => !allowed.has(k))).toEqual([]);
  });
});
