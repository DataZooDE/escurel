import { describe, expect, it } from 'vitest';
import { existsSync, readFileSync, readdirSync, statSync } from 'node:fs';
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
    commands: { command: string; icon?: string }[];
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
        // Inline AND in the context menu: an icon alone is out of reach of a keyboard-only user.
        'escurel.approvePlan',
        'escurel.approvePlan',
        'escurel.cancelRun',
        'escurel.cancelRun',
        'escurel.pauseDispatch',
        'escurel.requeue',
        'escurel.resumeDispatch',
        'escurel.retryRun',
        'escurel.retryRun',
        'escurel.runs.clearFilter',
        'escurel.runs.copyRunId',
        'escurel.runs.filter',
        'escurel.runs.openTarget',
        'escurel.runs.openThread',
        'escurel.runs.refresh',
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

describe('inline buttons', () => {
  // A button shown inline on a tree row has no label, only its icon: a command with none renders as
  // nothing at all, and the action is simply not there. Nothing else catches it, since the command is
  // declared, registered and in the menu.
  it('every command shown inline in a view has an icon', () => {
    const inline = Object.values(manifest.contributes.menus)
      .flat()
      .filter((m) => m.group === 'inline' || m.group?.startsWith('inline@'))
      .map((m) => m.command);
    expect(inline.length).toBeGreaterThan(0);
    const withoutIcon = [...new Set(inline)].filter(
      (id) => !manifest.contributes.commands.find((c) => c.command === id)?.icon,
    );
    expect(withoutIcon).toEqual([]);
  });

  // The same is true of a view's title bar: its actions are icons, and one without an icon is drawn
  // as its TITLE TEXT instead, which overlaps its neighbour ("Pause disp...Resume dispatch" in the
  // Runner view, seen in a screenshot of the real window).
  it('every action in a view title bar has an icon', () => {
    const inTitle = (manifest.contributes.menus['view/title'] ?? []).map((m) => m.command);
    expect(inTitle.length).toBeGreaterThan(0);
    const withoutIcon = [...new Set(inTitle)].filter(
      (id) => !manifest.contributes.commands.find((c) => c.command === id)?.icon,
    );
    expect(withoutIcon).toEqual([]);
  });
});

describe('settings that decide where a credential goes or what runs', () => {
  // A setting with no scope can be set by a repository's .vscode/settings.json, and once the user
  // clicks "Trust" it is honoured. For these that means the user's bearer token goes to a host the
  // repository chose, or a command it chose runs. Restricted Mode does not help: it is the OTHER
  // case. They are user-level (application) settings only.
  it.each([
    'escurel.gatewayUrl',
    'escurel.auth.issuer',
    'escurel.auth.clientId',
    'escurel.auth.scopes',
    'escurel.shellHarness',
    'escurel.harness',
  ])('%s cannot be set by a workspace', (id) => {
    const prop = manifest.contributes.configuration.properties[id] as { scope?: string };
    expect(prop.scope).toBe('application');
  });
});

describe('command titles', () => {
  // The webviews, the Skill menu and the spec all use sentence case ("Start in background", "Cancel
  // run"); the palette and the menus used to mix it with Title Case ("Open Page" next to "Open
  // instance"). One style, so the same action reads the same on every surface.
  it('are in sentence case: no word after the first starts with a capital, bar proper nouns', () => {
    const proper = new Set(['Markdown', 'Escurel']);
    const bad = (manifest.contributes.commands as unknown as { title: string }[])
      .map((c) => c.title)
      .filter((title: string) =>
        title
          .split(/\s+/)
          .slice(1)
          .some((w) => /^[A-Z][a-z]/.test(w) && !proper.has(w.replace(/[….]$/, ''))),
      );
    expect(bad).toEqual([]);
  });
});

describe('the details view', () => {
  // The thread's details are a view of their own in VS Code's PANEL area, so the user docks, moves
  // and resizes it with VS Code's own layout (owner: "use native layout mechanisms"), not a column
  // inside the canvas webview.
  const contributes = manifest.contributes as unknown as {
    viewsContainers: Record<string, { id: string; title: string; icon: string }[]>;
    views: Record<string, { id: string; name: string; type?: string }[]>;
  };

  it('is a webview view in a panel container, with an icon that exists', () => {
    const container = contributes.viewsContainers.panel?.find((c) => c.id === 'escurel-details');
    expect(container?.title).toBe('Escurel Details');
    expect(existsSync(join(__dirname, '../../', container?.icon ?? 'missing'))).toBe(true);
    expect(contributes.views['escurel-details']).toEqual([
      expect.objectContaining({ id: 'escurel.details', type: 'webview' }),
    ]);
  });
});

describe('the empty views say WHY they are empty', () => {
  // `Not connected` for every failure left a person with a quarantined tenant or an outdated gateway
  // clicking Reconnect. The host publishes `escurel.connectionState`; each state has its own words.
  const welcome = (
    manifest.contributes as unknown as {
      viewsWelcome: { view: string; contents: string; when: string }[];
    }
  ).viewsWelcome;
  for (const view of ['escurel.knowledge', 'escurel.awaiting', 'escurel.inbox', 'escurel.runner']) {
    it(`${view} has a message for a quarantined tenant and an outdated gateway`, () => {
      const forState = (state: string) =>
        welcome.find(
          (w) => w.view === view && w.when.includes(`escurel.connectionState == '${state}'`),
        );
      expect(forState('quarantined')?.contents).toContain('escurel admin migrate-kind');
      expect(forState('incompatible')?.contents).toMatch(/older than this extension/i);
    });

    it(`${view} keeps the generic Reconnect message for the other failures only`, () => {
      const generic = welcome.filter(
        (w) =>
          w.view === view &&
          !w.when.includes("== 'quarantined'") &&
          !w.when.includes("== 'incompatible'"),
      );
      expect(generic.length).toBeGreaterThan(0);
      for (const g of generic) {
        expect(g.when).toContain("escurel.connectionState != 'quarantined'");
        expect(g.when).toContain("escurel.connectionState != 'incompatible'");
      }
    });
  }
});

describe('first run and discoverability', () => {
  const m = manifest as unknown as {
    contributes: {
      commands: { command: string }[];
      keybindings: { command: string }[];
      walkthroughs: {
        steps: { description: string; media: { markdown: string }; completionEvents?: string[] }[];
      }[];
      viewsWelcome: { view: string; contents: string }[];
      menus: { commandPalette: { command: string; when: string }[] };
    };
    activationEvents: string[];
  };
  const declared = new Set(m.contributes.commands.map((c) => c.command));
  // A command: link may call one of ours, a VS Code built-in, or a view's auto-generated focus command.
  const known = (id: string) =>
    declared.has(id) || id.startsWith('workbench.') || id.endsWith('.focus');

  it('every command a welcome text, walkthrough step or keybinding runs exists', () => {
    const uris = [
      ...m.contributes.viewsWelcome.map((w) => w.contents),
      ...m.contributes.walkthroughs.flatMap((w) => w.steps.map((s) => s.description)),
    ].flatMap((text) => [...text.matchAll(/command:([A-Za-z0-9_.]+)/g)].map((x) => x[1]!));
    expect(uris.length > 0).toBe(true);
    expect(uris.filter((u) => !known(u))).toEqual([]);
    expect(m.contributes.keybindings.map((k) => k.command).filter((c) => !known(c))).toEqual([]);
  });

  it('a not-connected view tells you HOW to connect: a link that opens the gateway setting', () => {
    // The quarantined and old-gateway states have their own words (the setting is not what is wrong).
    const generic = (m.contributes.viewsWelcome as { when?: string; contents: string }[]).filter(
      (w) =>
        !(w.when ?? '').includes("== 'quarantined'") &&
        !(w.when ?? '').includes("== 'incompatible'"),
    );
    expect(generic.length > 0).toBe(true);
    for (const w of generic) {
      expect(w.contents).toContain('command:workbench.action.openSettings');
      expect(w.contents).toContain('escurel.gatewayUrl');
    }
  });

  it('the walkthrough ships its pages', () => {
    const dir = new URL('../../', import.meta.url).pathname;
    for (const step of m.contributes.walkthroughs.flatMap((w) => w.steps)) {
      expect(existsSync(join(dir, step.media.markdown))).toBe(true);
    }
  });

  it('the commands people ran from the palette are no longer hidden from it', () => {
    const hidden = new Set(
      m.contributes.menus.commandPalette.filter((c) => c.when === 'false').map((c) => c.command),
    );
    for (const id of [
      'escurel.cancelRun',
      'escurel.retryRun',
      'escurel.approvePlan',
      'escurel.openThread',
      'escurel.openReview',
    ]) {
      expect(hidden.has(id)).toBe(false);
    }
  });

  it('the bottom Details panel and the Runner can be opened by name, and wake the extension', () => {
    expect(declared.has('escurel.showDetails')).toBe(true);
    expect(declared.has('escurel.showRunner')).toBe(true);
    expect(m.activationEvents).toContain('onView:escurel.details');
    expect(m.activationEvents).toContain('onView:escurel.runner');
  });
});
