import { describe, expect, it, vi } from 'vitest';
import * as vscode from 'vscode';
import {
  BUILTIN_PROFILE,
  FocusProfileRegistry,
  chooseProfile,
  validateProfile,
} from '../../src/focus/focusProfiles';
import { FOCUS_SETTINGS, planEnter, planExit } from '../../src/focus/focusSettings';
import { FocusMode } from '../../src/focus/focusMode';
import { createContributions, createExtensionApi } from '../../src/api';

const A = { id: 'acme.calm', settings: { 'window.title': 'Acme', 'breadcrumbs.enabled': false } };
const B = {
  id: 'other.quiet',
  settings: { 'window.title': 'Other', 'editor.minimap.enabled': false },
};

describe('validateProfile', () => {
  it('accepts a profile of VS Code appearance settings', () => {
    expect(validateProfile(A)).toEqual(A);
    expect(
      validateProfile({ id: 'a-b_c.d', settings: { 'workbench.colorTheme': 'Acme' } }),
    ).toBeDefined();
  });

  it.each([
    ['no object', undefined],
    ['an empty id', { id: '', settings: { 'window.title': 'x' } }],
    ['an id with a space', { id: 'a b', settings: { 'window.title': 'x' } }],
    ['the built-in id', { id: BUILTIN_PROFILE, settings: { 'window.title': 'x' } }],
    ['no settings', { id: 'a', settings: {} }],
    ['settings that are a list', { id: 'a', settings: [] }],
    [
      'a setting of this extension',
      { id: 'a', settings: { 'escurel.gatewayUrl': 'http://evil.example' } },
    ],
    ['a terminal setting', { id: 'a', settings: { 'terminal.integrated.shell.linux': '/bin/sh' } }],
    ['a trust setting', { id: 'a', settings: { 'workbench.trustedDomains': ['x'] } }],
    ['a security setting', { id: 'a', settings: { 'security.workspace.trust.enabled': false } }],
    ['a command-like setting', { id: 'a', settings: { 'window.openWithCommand': 'x' } }],
    ['an editor setting off the list', { id: 'a', settings: { 'editor.formatOnSave': true } }],
    ['a key with a space', { id: 'a', settings: { 'window.ti tle': 'x' } }],
    [
      'a prototype key',
      { id: 'a', settings: JSON.parse('{"__proto__":{"x":1},"window.title":"x"}') },
    ],
    ['a function value', { id: 'a', settings: { 'window.title': () => 'x' } }],
    ['an undefined value', { id: 'a', settings: { 'window.title': undefined } }],
    ['a huge value', { id: 'a', settings: { 'window.title': 'x'.repeat(2000) } }],
  ])('refuses %s', (_n, p) => {
    expect(() => validateProfile(p as never)).toThrow();
  });

  it('refuses more than 40 settings', () => {
    const settings = Object.fromEntries(
      Array.from({ length: 41 }, (_, i) => [`workbench.k${i}`, true]),
    );
    expect(() => validateProfile({ id: 'a', settings })).toThrow(/at most/);
  });

  it('copies the settings, so a later change by the caller does not reach the host', () => {
    const src = { id: 'a', settings: { 'window.title': 'x', 'workbench.k': { n: [1] } } };
    const p = validateProfile(src);
    (src.settings['workbench.k'] as { n: number[] }).n.push(2);
    src.settings['window.title'] = 'changed';
    expect(p.settings).toEqual({ 'window.title': 'x', 'workbench.k': { n: [1] } });
    expect(Object.isFrozen(p)).toBe(true);
  });
});

describe('the registry', () => {
  it('registers, lists, refuses a duplicate and unregisters on dispose', () => {
    const r = new FocusProfileRegistry();
    const changed = vi.fn();
    r.onDidChange(changed);
    const d = r.register(A);
    expect(r.ids()).toEqual(['acme.calm']);
    expect(r.get('acme.calm')?.settings['window.title']).toBe('Acme');
    expect(() => r.register(A)).toThrow(/already/);
    d.dispose();
    expect(r.ids()).toEqual([]);
    expect(changed).toHaveBeenCalledTimes(2);
    d.dispose();
    expect(changed).toHaveBeenCalledTimes(2);
  });

  it('does not let an old disposable remove a profile registered later under the same id', () => {
    const r = new FocusProfileRegistry();
    const d1 = r.register(A);
    d1.dispose();
    r.register({ ...A, settings: { 'window.title': 'New' } });
    d1.dispose();
    expect(r.get('acme.calm')?.settings['window.title']).toBe('New');
  });

  it('has no inherited ids', () => {
    expect(new FocusProfileRegistry().get('__proto__')).toBeUndefined();
    expect(new FocusProfileRegistry().get('constructor')).toBeUndefined();
  });
});

describe('which profile is active', () => {
  it('is the built-in one when nothing is registered', () => {
    expect(chooseProfile('', [])).toBe(BUILTIN_PROFILE);
    expect(chooseProfile(undefined, [])).toBe(BUILTIN_PROFILE);
  });
  it('is the companion profile when exactly one is registered and the user chose none', () => {
    expect(chooseProfile('', ['acme.calm'])).toBe('acme.calm');
  });
  it('stays with the built-in one when two are registered and the user chose none', () => {
    expect(chooseProfile('', ['acme.calm', 'other.quiet'])).toBe(BUILTIN_PROFILE);
  });
  it('follows the user setting, including "escurel" for the built-in one', () => {
    expect(chooseProfile('other.quiet', ['acme.calm', 'other.quiet'])).toBe('other.quiet');
    expect(chooseProfile(BUILTIN_PROFILE, ['acme.calm'])).toBe(BUILTIN_PROFILE);
  });
  it('ignores a setting that names a profile nobody registered', () => {
    expect(chooseProfile('gone.profile', ['acme.calm'])).toBe('acme.calm');
    expect(chooseProfile('gone.profile', ['acme.calm', 'other.quiet'])).toBe(BUILTIN_PROFILE);
  });
});

describe('planEnter with a profile: two extensions must not clobber each other', () => {
  const user = { 'window.title': 'My window', 'breadcrumbs.enabled': true };

  it('writes the profile, not the built-in settings, and remembers the person own values', () => {
    const plan = planEnter(user, undefined, A.settings);
    expect(plan.writes).toEqual(A.settings);
    expect(plan.saved).toEqual({
      'window.title': { set: true, value: 'My window' },
      'breadcrumbs.enabled': { set: true, value: true },
    });
  });

  it('with no profile argument it is the built-in behaviour', () => {
    expect(planEnter({}, undefined).writes).toEqual(FOCUS_SETTINGS);
  });

  it('switching profile keeps the first originals for shared keys, saves new keys, releases dropped ones', () => {
    const first = planEnter(user, undefined, A.settings);
    // The window now shows A's values; the person never set the minimap key.
    const shown = { ...user, ...A.settings };
    const second = planEnter(shown, first.saved, B.settings);
    // 'window.title' is shared: the original is still the person's, never A's 'Acme'.
    expect(second.saved['window.title']).toEqual({ set: true, value: 'My window' });
    // 'editor.minimap.enabled' is new: its original is "never set".
    expect(second.saved['editor.minimap.enabled']).toEqual({ set: false });
    // 'breadcrumbs.enabled' is A's only: it is given back now.
    expect(second.writes['breadcrumbs.enabled']).toBe(true);
    expect(second.saved['breadcrumbs.enabled']).toBeUndefined();
    expect(second.writes['window.title']).toBe('Other');
    // Leaving puts the person's values back for everything B touched.
    expect(planExit(second.saved)).toEqual({
      'window.title': 'My window',
      'editor.minimap.enabled': undefined,
    });
  });

  it('a key released by a switch that the person never set is removed', () => {
    const first = planEnter({}, undefined, A.settings);
    const second = planEnter({ ...A.settings }, first.saved, B.settings);
    expect(second.writes).toHaveProperty('breadcrumbs.enabled', undefined);
  });

  it('re-entering the same profile changes nothing remembered', () => {
    const first = planEnter(user, undefined, A.settings);
    const again = planEnter({ ...user, ...A.settings }, first.saved, A.settings);
    expect(again.saved).toEqual(first.saved);
    expect(again.writes).toEqual(A.settings);
  });
});

describe('FocusMode applies the active profile in a window', () => {
  function window(initial: Record<string, unknown> = {}) {
    const store: Record<string, unknown> = { ...initial };
    const memento = new Map<string, unknown>();
    const cfg = {
      inspect: (k: string) => ({ globalValue: store[k] }),
      update: async (k: string, v: unknown) => {
        // A real settings write takes time: concurrent applies must not read each other's half-written state.
        await new Promise((r) => setTimeout(r, 1));
        if (v === undefined) delete store[k];
        else store[k] = v;
      },
      get: (k: string) => store[k],
    };
    vi.spyOn(vscode.workspace, 'getConfiguration').mockReturnValue(cfg as never);
    const registry = new FocusProfileRegistry();
    let configured = '';
    const focus = new FocusMode(
      {
        get: (k: string) => memento.get(k),
        update: async (k: string, v: unknown) =>
          void (v === undefined ? memento.delete(k) : memento.set(k, v)),
      } as never,
      { registry, configured: () => configured },
    );
    return { store, registry, focus, choose: (id: string) => (configured = id) };
  }

  it('applies the one registered companion profile, and exit restores the person', async () => {
    const { store, registry, focus } = window({ 'window.title': 'Mine' });
    registry.register(A);
    await focus.enter({ silent: true, overview: false });
    expect(store['window.title']).toBe('Acme');
    expect(store['breadcrumbs.enabled']).toBe(false);
    expect(store['workbench.statusBar.visible']).toBeUndefined();
    await focus.exit();
    expect(store).toEqual({ 'window.title': 'Mine' });
  });

  it('conflict: two profiles, the user picks one, a switch while on does not lose the person values', async () => {
    const { store, registry, focus, choose } = window({
      'window.title': 'Mine',
      'breadcrumbs.enabled': true,
    });
    registry.register(A);
    registry.register(B);
    await focus.enter({ silent: true, overview: false });
    // Two registered, none chosen: the built-in profile, neither companion wins.
    expect(store['window.title']).toBe('Escurel');
    choose('acme.calm');
    await focus.reapply();
    expect(store['window.title']).toBe('Acme');
    choose('other.quiet');
    await focus.reapply();
    expect(store['window.title']).toBe('Other');
    expect(store['breadcrumbs.enabled']).toBe(true);
    expect(store['editor.minimap.enabled']).toBe(false);
    expect(store['workbench.statusBar.visible']).toBeUndefined();
    await focus.exit();
    expect(store).toEqual({ 'window.title': 'Mine', 'breadcrumbs.enabled': true });
  });

  it('two reapplies at once (a setting event may fire twice) still remember the person values', async () => {
    const { store, registry, focus, choose } = window({ 'window.title': 'Mine' });
    registry.register(A);
    registry.register(B);
    choose('acme.calm');
    await focus.enter({ silent: true, overview: false });
    choose('other.quiet');
    await Promise.all([focus.reapply(), focus.reapply(), focus.reapply()]);
    expect(store['editor.minimap.enabled']).toBe(false);
    await focus.exit();
    expect(store).toEqual({ 'window.title': 'Mine' });
  });

  it('a reapply that arrives while exit is writing does not put the calm window back', async () => {
    const { store, registry, focus } = window({ 'window.title': 'Mine' });
    registry.register(A);
    await focus.enter({ silent: true, overview: false });
    await Promise.all([focus.exit(), focus.reapply(), focus.reapply()]);
    expect(store).toEqual({ 'window.title': 'Mine' });
    expect(focus.isOn()).toBe(false);
  });

  it('does not depend on the memento showing a write at once (VS Code global state may lag)', async () => {
    const store: Record<string, unknown> = { 'window.title': 'Mine' };
    const persisted = new Map<string, unknown>();
    vi.spyOn(vscode.workspace, 'getConfiguration').mockReturnValue({
      inspect: (k: string) => ({ globalValue: store[k] }),
      update: async (k: string, v: unknown) =>
        void (v === undefined ? delete store[k] : (store[k] = v)),
    } as never);
    const registry = new FocusProfileRegistry();
    registry.register(A);
    registry.register(B);
    let chosen = 'acme.calm';
    // `get` keeps answering with what was there when the window started: a lagging store.
    const focus = new FocusMode(
      {
        get: () => undefined,
        update: async (k: string, v: unknown) => void persisted.set(k, v),
      } as never,
      { registry, configured: () => chosen },
    );
    await focus.enter({ silent: true, overview: false });
    expect(focus.isOn()).toBe(true);
    chosen = 'other.quiet';
    await focus.reapply();
    expect(store['window.title']).toBe('Other');
    await focus.exit();
    expect(store).toEqual({ 'window.title': 'Mine' });
  });

  it('reapply does nothing when focus mode is off', async () => {
    const { store, registry, focus } = window({ 'window.title': 'Mine' });
    registry.register(A);
    await focus.reapply();
    expect(store).toEqual({ 'window.title': 'Mine' });
  });

  it('disposing the active profile while on falls back to the built-in one', async () => {
    const { store, registry, focus } = window({ 'window.title': 'Mine' });
    const d = registry.register(A);
    await focus.enter({ silent: true, overview: false });
    d.dispose();
    await focus.reapply();
    expect(store['window.title']).toBe('Escurel');
    expect(store['workbench.statusBar.visible']).toBe(false);
    await focus.exit();
    expect(store).toEqual({ 'window.title': 'Mine' });
  });
});

describe('the API member', () => {
  it('registers a profile in the host registry, validates and unregisters', () => {
    const changed = new vscode.EventEmitter<void>();
    const deps = {
      client: () => ({ callTool: <T>() => Promise.resolve(undefined as T) }),
      gatewayUrl: () => 'http://gw.test',
      onDidChange: changed.event,
    };
    const c = createContributions(deps);
    const api = createExtensionApi(deps, c);
    const d = api.registerFocusProfile(A);
    expect(c.focusProfiles.ids()).toEqual(['acme.calm']);
    expect(() => api.registerFocusProfile(A)).toThrow(/already/);
    expect(() =>
      api.registerFocusProfile({
        id: 'x',
        settings: { 'escurel.gatewayUrl': 'http://evil.example' },
      }),
    ).toThrow();
    d.dispose();
    expect(c.focusProfiles.ids()).toEqual([]);
  });
});
