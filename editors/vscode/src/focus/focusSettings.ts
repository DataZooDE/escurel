// Focus mode, pure: which VS Code settings make the window calm, and how to put a person's own back.
// No `vscode` import (the host half is focusMode.ts), so the round trip is unit-tested.

/** The colour theme the extension ships (`contributes.themes`). */
export const FOCUS_THEME = 'Escurel Calm';

/**
 * What the calm window sets. Each one was tried on a real VS Code (1.140) window: the menu bar, command
 * center, status bar, breadcrumbs and minimap go; the title bar stays (Linux ignores
 * `window.customTitleBarVisibility`) but reads "Escurel" instead of "folder - Visual Studio Code".
 */
export const FOCUS_SETTINGS: Readonly<Record<string, unknown>> = {
  'window.menuBarVisibility': 'hidden',
  'window.commandCenter': false,
  'window.title': 'Escurel',
  'workbench.statusBar.visible': false,
  'workbench.activityBar.location': 'top',
  'workbench.layoutControl.enabled': false,
  'workbench.editor.editorActionsLocation': 'hidden',
  'workbench.colorTheme': FOCUS_THEME,
  'breadcrumbs.enabled': false,
  'editor.minimap.enabled': false,
};

/** One setting as the person had it: set (with its value) or never set at user level. */
export type SavedSettings = Record<string, { set: boolean; value?: unknown }>;

export interface EnterPlan {
  /** What to remember BEFORE writing. */
  saved: SavedSettings;
  /** What to write. */
  writes: Record<string, unknown>;
}

/**
 * Entering the calm window. `current` is each key's user-level value (absent when never set);
 * `alreadySaved` is what an earlier enter remembered. The originals are the FIRST ones: a second
 * enter sees the focus values in the window and must not mistake them for the person's own.
 */
export function planEnter(
  current: Readonly<Record<string, unknown>>,
  alreadySaved: SavedSettings | undefined,
  settings: Readonly<Record<string, unknown>> = FOCUS_SETTINGS,
): EnterPlan {
  const had: SavedSettings = alreadySaved ?? {};
  const saved: SavedSettings = {};
  const writes: Record<string, unknown> = { ...settings };
  for (const key of Object.keys(settings)) {
    // A key that an earlier enter already remembered keeps its FIRST original; a key new to this profile
    // is remembered as the window has it now (nobody of ours wrote it yet).
    saved[key] =
      had[key] ??
      (key in current && current[key] !== undefined
        ? { set: true, value: current[key] }
        : { set: false });
  }
  // Switching profile while on: what the previous profile changed and this one does not is given back.
  for (const [key, was] of Object.entries(had)) {
    if (!(key in settings)) writes[key] = was.set ? was.value : undefined;
  }
  return { saved, writes };
}

/** Leaving it: the value to write for each key (`undefined` removes the key), or nothing if none was saved. */
export function planExit(saved: SavedSettings | undefined): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  if (!saved) return out;
  for (const [key, was] of Object.entries(saved)) out[key] = was.set ? was.value : undefined;
  return out;
}
