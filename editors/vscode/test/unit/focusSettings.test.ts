import { describe, expect, it } from 'vitest';
import {
  FOCUS_SETTINGS,
  FOCUS_THEME,
  planEnter,
  planExit,
  type SavedSettings,
} from '../../src/focus/focusSettings';

describe('focus mode settings', () => {
  it('hides the IDE chrome and nothing else a person reads', () => {
    expect(FOCUS_SETTINGS['window.menuBarVisibility']).toBe('hidden');
    expect(FOCUS_SETTINGS['window.commandCenter']).toBe(false);
    expect(FOCUS_SETTINGS['workbench.statusBar.visible']).toBe(false);
    expect(FOCUS_SETTINGS['breadcrumbs.enabled']).toBe(false);
    expect(FOCUS_SETTINGS['editor.minimap.enabled']).toBe(false);
    expect(FOCUS_SETTINGS['workbench.activityBar.location']).toBe('top');
    expect(FOCUS_SETTINGS['window.title']).toBe('Escurel');
    expect(FOCUS_SETTINGS['workbench.colorTheme']).toBe(FOCUS_THEME);
  });

  it('only ever names settings that VS Code owns', () => {
    for (const key of Object.keys(FOCUS_SETTINGS)) {
      expect(key).toMatch(/^(window|workbench|breadcrumbs|editor)\./);
    }
  });

  it('remembers what the person had, including the keys they had never set', () => {
    const plan = planEnter(
      { 'window.zoomLevel': 2, 'workbench.colorTheme': 'Default Dark Modern' },
      undefined,
    );
    expect(plan.saved['window.zoomLevel']).toEqual({ set: true, value: 2 });
    expect(plan.saved['workbench.colorTheme']).toEqual({ set: true, value: 'Default Dark Modern' });
    expect(plan.saved['workbench.statusBar.visible']).toEqual({ set: false });
    expect(plan.writes).toEqual(FOCUS_SETTINGS);
  });

  it('a second enter keeps the FIRST originals', () => {
    const first = planEnter({ 'window.zoomLevel': 2 }, undefined);
    // Now the window already shows the focus values; entering again must not save THOSE as the originals.
    const second = planEnter({ ...FOCUS_SETTINGS }, first.saved);
    expect(second.saved).toEqual(first.saved);
  });

  it('exit puts every value back, and removes a key that was never set', () => {
    const { saved } = planEnter({ 'window.zoomLevel': 2 }, undefined);
    const back = planExit(saved);
    expect(back['window.zoomLevel']).toBe(2);
    expect(back['workbench.statusBar.visible']).toBeUndefined();
    expect(Object.keys(back).sort()).toEqual(Object.keys(FOCUS_SETTINGS).sort());
  });

  it('exit without anything saved changes nothing', () => {
    expect(planExit(undefined)).toEqual({});
    expect(planExit({} as SavedSettings)).toEqual({});
  });
});
