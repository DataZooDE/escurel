// One source for the demo/workbench window settings: demo/settings.common.json is shared by the desktop demo
// (render-demo-settings.mjs, run by demo/run.sh) and the web workbench image (render-settings.mjs, run by its
// entrypoint). These tests pin what each one renders and that the focus-mode keys agree with the extension's own.
import { spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import { FOCUS_SETTINGS } from '../../src/focus/focusSettings';

const demo = join(__dirname, '../../demo');
const web = join(__dirname, '../../../../deploy/web-workbench');
const common = JSON.parse(readFileSync(join(demo, 'settings.common.json'), 'utf8')) as Record<
  string,
  unknown
>;
const webBase = JSON.parse(readFileSync(join(web, 'settings.base.json'), 'utf8')) as Record<
  string,
  unknown
>;

async function renderDemo(env: Record<string, string>): Promise<Record<string, unknown>> {
  // @ts-expect-error plain ES module without types
  const { renderDemoSettings } = await import('../../demo/render-demo-settings.mjs');
  return renderDemoSettings(common, env) as Record<string, unknown>;
}

function renderWeb(): Record<string, unknown> {
  const out = mkdtempSync(join(tmpdir(), 'escurel-web-settings-'));
  const r = spawnSync(
    process.execPath,
    [
      join(web, 'render-settings.mjs'),
      join(demo, 'settings.common.json'),
      join(web, 'settings.base.json'),
      join(web, 'keybindings.json'),
      out,
    ],
    {
      env: { PATH: process.env.PATH ?? '', WORKBENCH_GATEWAY_URL: 'http://escurel:8080' },
      encoding: 'utf8',
    },
  );
  expect(r.status, r.stderr).toBe(0);
  return JSON.parse(readFileSync(join(out, 'User', 'settings.json'), 'utf8')) as Record<
    string,
    unknown
  >;
}

describe('window settings have one shared source', () => {
  it('the shared keys are in neither private file, so they cannot drift', () => {
    for (const key of Object.keys(common)) expect(webBase, key).not.toHaveProperty(key);
  });

  it('the desktop demo renders the shared keys plus its own', async () => {
    const s = await renderDemo({ ESCUREL_DEMO_GATEWAY_URL: 'http://127.0.0.1:1234' });
    expect(s).toMatchObject(common);
    expect(s).toMatchObject({
      'escurel.gatewayUrl': 'http://127.0.0.1:1234',
      'escurel.evolveEndpoint': '',
      'window.restoreWindows': 'none',
      'window.zoomLevel': 1,
      'window.dialogStyle': 'native',
    });
    expect(s).not.toHaveProperty('workbench.colorTheme');
  });

  it('the desktop demo takes zoom, dialog style and theme from the environment, and needs a gateway', async () => {
    const s = await renderDemo({
      ESCUREL_DEMO_GATEWAY_URL: 'http://g',
      ESCUREL_DEMO_ZOOM: '0',
      ESCUREL_DEMO_DIALOG_STYLE: 'custom',
      ESCUREL_DEMO_THEME: 'Escurel Calm',
      ESCUREL_DEMO_EVOLVE_ENDPOINT: 'http://evolve',
    });
    expect(s).toMatchObject({
      'window.zoomLevel': 0,
      'window.dialogStyle': 'custom',
      'workbench.colorTheme': 'Escurel Calm',
      'escurel.evolveEndpoint': 'http://evolve',
    });
    await expect(renderDemo({})).rejects.toThrow(/ESCUREL_DEMO_GATEWAY_URL/);
  });

  it('the web workbench renders the shared keys plus its base and the gateway address', () => {
    const s = renderWeb();
    expect(s).toMatchObject(common);
    expect(s).toMatchObject({ ...webBase, 'escurel.gatewayUrl': 'http://escurel:8080' });
  });

  it("the web workbench agrees with the extension's focus mode on every focus key but the activity bar", () => {
    const s = renderWeb();
    const differs = Object.entries(FOCUS_SETTINGS).filter(
      ([k, v]) => JSON.stringify(s[k]) !== JSON.stringify(v),
    );
    // The image has no activity bar at all (hidden); focus mode on a person's own window keeps one at the top.
    expect(differs).toEqual([['workbench.activityBar.location', 'top']]);
  });
});
