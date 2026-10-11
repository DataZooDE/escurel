import { beforeEach, describe, expect, it, vi } from 'vitest';
import * as vscode from 'vscode';
import { registerFocusSuggestion } from '../../src/focus/registerSuggestion';
import { SUGGESTED_KEY } from '../../src/focus/focusSuggestion';

vi.mock('vscode', () => ({
  workspace: { isTrusted: true, workspaceFolders: [] },
  env: { uiKind: 1 }, UIKind: { Web: 2 },
  extensions: { getExtension: vi.fn() },
  window: { showInformationMessage: vi.fn(async () => undefined) },
  commands: { executeCommand: vi.fn(async () => undefined) },
}));

function setup() {
  let connected: (() => Promise<void>) | undefined;
  const saved = new Map<string, unknown>();
  const context = { globalState: {
    get: (key: string) => saved.get(key),
    update: async (key: string, value: unknown) => { saved.set(key, value); },
  }, subscriptions: [] };
  const focus = { isOn: () => false, silentBootstrap: false };
  const socket = {};
  const live = { socketState: 'open', activeSocket: socket,
    onDidConnect: (listener: () => Promise<void>) => { connected = listener; return { dispose() {} }; },
  };
  const services = { subject: async () => 'alice' as string | undefined };
  // Only the host boundary is substituted; exercise the real decision and persistence orchestration.
  registerFocusSuggestion(context as never, focus as never, services as never, live as never);
  return { saved, focus, live, services, connect: () => connected!() };
}

beforeEach(() => {
  vi.clearAllMocks();
  Object.assign(vscode.workspace, { isTrusted: true, workspaceFolders: [] });
  Object.assign(vscode.env, { uiKind: 1 });
  vi.mocked(vscode.extensions.getExtension).mockReturnValue(undefined);
});

describe('focus suggestion host wiring', () => {
  it('suggests after a signed-in connection, persists once, and executes the chosen command', async () => {
    vi.mocked(vscode.window.showInformationMessage).mockResolvedValue('Switch to focus view' as never);
    const test = setup();
    await test.connect();
    await test.connect();
    expect(test.saved.get(SUGGESTED_KEY)).toBe(true);
    expect(vscode.window.showInformationMessage).toHaveBeenCalledTimes(1);
    expect(vscode.commands.executeCommand).toHaveBeenCalledWith('escurel.focusMode.enter', { silent: true });
  });

  for (const reason of ['unsigned', 'disconnected', 'focus', 'silent', 'untrusted', 'virtual', 'web', 'demo', 'workbench', 'previous']) {
    it(`suppresses the notice for ${reason}`, async () => {
      const test = setup();
      if (reason === 'unsigned') test.services.subject = async () => undefined;
      if (reason === 'disconnected') test.live.socketState = 'closed';
      if (reason === 'focus') test.focus.isOn = () => true;
      if (reason === 'silent') test.focus.silentBootstrap = true;
      if (reason === 'untrusted') Object.assign(vscode.workspace, { isTrusted: false });
      if (reason === 'virtual') Object.assign(vscode.workspace, { workspaceFolders: [{ uri: { scheme: 'github' } }] });
      if (reason === 'web') Object.assign(vscode.env, { uiKind: 2 });
      if (reason === 'demo' || reason === 'workbench') {
        vi.mocked(vscode.extensions.getExtension).mockImplementation(id =>
          id === `datazoo.escurel-${reason === 'demo' ? 'demo' : 'web'}-bootstrap` ? {} as never : undefined);
      }
      if (reason === 'previous') test.saved.set(SUGGESTED_KEY, true);
      await test.connect();
      expect(vscode.window.showInformationMessage).not.toHaveBeenCalled();
    });
  }

  it('ignores a connection replaced while reading the signed-in subject', async () => {
    const test = setup();
    test.services.subject = async () => { test.live.activeSocket = {}; return 'alice'; };
    await test.connect();
    expect(vscode.window.showInformationMessage).not.toHaveBeenCalled();
  });
});
