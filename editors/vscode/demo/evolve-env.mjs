// The environment the demo's Evolve agent (anofox-evolve `evolve-agent serve`) runs with, in ONE place: the
// desktop demo (run.sh) and the end-to-end fixtures (test/e2e/fixtures.ts) both start the agent against a demo
// gateway's token issuer and used to carry two copies of this block.
//
// The agent verifies tokens against the issuer in OIDC mode. Its `ESCUREL_OIDC_*` keys look like the gateway's
// (`ESCUREL_AUTH_OIDC_*`) but belong to the AGENT; they are being renamed `ESCUREL_EVOLVE_OIDC_*` (the old names
// stay a deprecated alias for one release), so both spellings are passed: whichever the agent build reads wins.
//
//   node evolve-env.mjs lines <gateway_url> <admin_bearer> <issuer_url> <tenant>   one KEY=VALUE per line (run.sh)
export function evolveAgentEnv({ gatewayUrl, adminBearer, issuerUrl, tenant }) {
  const jwks = `${issuerUrl}/protocol/openid-connect/certs`;
  return {
    ESCUREL_ENDPOINT: gatewayUrl,
    ESCUREL_TOKEN: adminBearer,
    ESCUREL_OIDC_ISSUER: issuerUrl,
    ESCUREL_OIDC_AUDIENCE: 'escurel',
    ESCUREL_OIDC_JWKS_URI: jwks,
    ESCUREL_EVOLVE_OIDC_ISSUER: issuerUrl,
    ESCUREL_EVOLVE_OIDC_AUDIENCE: 'escurel',
    ESCUREL_EVOLVE_OIDC_JWKS_URI: jwks,
    EVOLVE_OIDC_ISSUER: issuerUrl,
    EVOLVE_OIDC_AUDIENCE: 'escurel',
    EVOLVE_OIDC_JWKS_URI: jwks,
    EVOLVE_TENANT: tenant,
    GEMINI_API_KEY: 'unused-scripted-demo-key',
    EVOLVE_ALLOW_SYNTHETIC_BRAIN: '1',
  };
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const [mode, gatewayUrl, adminBearer, issuerUrl, tenant] = process.argv.slice(2);
  if (mode !== 'lines' || !tenant) {
    console.error('usage: evolve-env.mjs lines <gateway_url> <admin_bearer> <issuer_url> <tenant>');
    process.exit(2);
  }
  for (const [k, v] of Object.entries(
    evolveAgentEnv({ gatewayUrl, adminBearer, issuerUrl, tenant }),
  )) {
    console.log(`${k}=${v}`);
  }
}
