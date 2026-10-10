// Writes the workbench's user settings and keybindings at container start.
//
//   settings.common.json + settings.base.json  (baked into the image)  +  the few values that come from the environment
//
// The environment may only carry the gateway address and, for a gateway that verifies tokens, the
// OIDC issuer and client id. There is no token variable on purpose: the extension takes a bearer only
// from its own sign-in (OIDC), never from a setting or the environment.
import { copyFileSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

// settings.common.json (shared with the desktop demo) first, then the web-only base on top.
const [common, base, keybindings, userDataDir] = process.argv.slice(2);
const settings = { ...JSON.parse(readFileSync(common, 'utf8')), ...JSON.parse(readFileSync(base, 'utf8')) };

function url(name, value, { required }) {
  if (!value) {
    if (required) fail(`${name} is required (the address of the escurel gateway, e.g. http://escurel:8080)`);
    return undefined;
  }
  let u;
  try {
    u = new URL(value);
  } catch {
    fail(`${name} is not a URL: ${value}`);
  }
  if (u.protocol !== 'http:' && u.protocol !== 'https:') fail(`${name} must be http(s)`);
  return value.replace(/\/+$/, '');
}
function fail(msg) {
  console.error(`escurel-web: ${msg}`);
  process.exit(64);
}

settings['escurel.gatewayUrl'] = url('WORKBENCH_GATEWAY_URL', process.env.WORKBENCH_GATEWAY_URL, { required: true });
const issuer = url('WORKBENCH_AUTH_ISSUER', process.env.WORKBENCH_AUTH_ISSUER, { required: false });
if (issuer) {
  settings['escurel.auth.issuer'] = issuer;
  if (process.env.WORKBENCH_AUTH_CLIENT_ID) settings['escurel.auth.clientId'] = process.env.WORKBENCH_AUTH_CLIENT_ID;
}

const user = join(userDataDir, 'User');
mkdirSync(user, { recursive: true });
writeFileSync(join(user, 'settings.json'), `${JSON.stringify(settings, null, 2)}\n`);
copyFileSync(keybindings, join(user, 'keybindings.json'));
