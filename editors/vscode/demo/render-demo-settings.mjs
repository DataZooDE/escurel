// Writes the desktop demo's user settings: the keys every Escurel demo window shares
// (settings.common.json, also baked into the web workbench image) plus the few that only a
// desktop demo window has. The demo profile is a throwaway one, so the file is overwritten.
//
//   node render-demo-settings.mjs <settings.common.json> <out settings.json>
// from the environment: ESCUREL_DEMO_GATEWAY_URL (required), ESCUREL_DEMO_EVOLVE_ENDPOINT,
// ESCUREL_DEMO_ZOOM, ESCUREL_DEMO_DIALOG_STYLE, ESCUREL_DEMO_THEME.
import { readFileSync, writeFileSync } from 'node:fs';

export function renderDemoSettings(common, env) {
  const gateway = env.ESCUREL_DEMO_GATEWAY_URL;
  if (!gateway) throw new Error('ESCUREL_DEMO_GATEWAY_URL is required');
  const zoom = Number(env.ESCUREL_DEMO_ZOOM ?? 1);
  if (!Number.isFinite(zoom))
    throw new Error(`ESCUREL_DEMO_ZOOM is not a number: ${env.ESCUREL_DEMO_ZOOM}`);
  const out = {
    'escurel.gatewayUrl': gateway,
    'escurel.evolveEndpoint': env.ESCUREL_DEMO_EVOLVE_ENDPOINT ?? '',
    ...common,
    'window.restoreWindows': 'none',
    'window.zoomLevel': zoom,
    'window.dialogStyle': env.ESCUREL_DEMO_DIALOG_STYLE || 'native',
  };
  // A colour theme for the window (a themed tour of every screen).
  if (env.ESCUREL_DEMO_THEME) out['workbench.colorTheme'] = env.ESCUREL_DEMO_THEME;
  return out;
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const [common, out] = process.argv.slice(2);
  const settings = renderDemoSettings(JSON.parse(readFileSync(common, 'utf8')), process.env);
  writeFileSync(out, `${JSON.stringify(settings, null, 2)}\n`);
}
