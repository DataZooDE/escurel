import type { GatewayInfo } from './gatewayInfo';

/**
 * The runner's environment for the cascade pass: MINTED mode.
 *
 * `ESCUREL_RUNNER_TOKEN` is removed on purpose. The runner gives a static token precedence over
 * the signing key, so one inherited from the developer's shell would silently turn this suite
 * into the static-bearer mode that cannot prove which run wrote what.
 */
export function runnerEnv(
  base: NodeJS.ProcessEnv,
  info: GatewayInfo,
  where: { port: number; dir: string },
): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = { ...base };
  delete env.ESCUREL_RUNNER_TOKEN;
  return {
    ...env,
    ESCUREL_RUNNER_GATEWAY_URL: info.gateway_url,
    ESCUREL_RUNNER_TENANT: info.tenant,
    ESCUREL_RUNNER_AUTH_ISSUER: info.issuer_url,
    ESCUREL_RUNNER_AUTH_KID: info.kid,
    ESCUREL_RUNNER_AUTH_SIGNING_KEY: info.signing_key,
    ESCUREL_RUNNER_HARNESS: 'echo',
    ESCUREL_RUNNER_LISTEN: `127.0.0.1:${where.port}`,
    ESCUREL_RUNNER_LEDGER_PATH: `${where.dir}/ledger.duckdb`,
    ESCUREL_RUNNER_POLL_INTERVAL: '250ms',
  };
}
