/** The one line of JSON `escurel-test-gateway` prints. */
export interface GatewayInfo {
  gateway_url: string;
  issuer_url: string;
  kid: string;
  signing_key: string;
  bearer: string;
  /** The same subject with the admin role: requeue, pause and resume are admin-only. */
  admin_bearer: string;
  tenant: string;
}

const FIELDS = [
  'gateway_url',
  'issuer_url',
  'kid',
  'signing_key',
  'bearer',
  'admin_bearer',
  'tenant',
] as const;

/**
 * Parse the one line `escurel-test-gateway` prints, refusing one that lacks a field.
 *
 * A binary built before a field existed prints a line without it, and a suite that needs the
 * field would skip itself (an empty bearer reads as "not provided") and look green. A missing
 * field is therefore an error that names the field and says to rebuild.
 */
export function parseGatewayInfo(line: string): GatewayInfo {
  let parsed: Record<string, unknown>;
  try {
    parsed = JSON.parse(line) as Record<string, unknown>;
  } catch {
    throw new Error(`escurel-test-gateway's first line is not JSON: ${line.slice(0, 200)}`);
  }
  for (const field of FIELDS) {
    const v = parsed[field];
    if (typeof v !== 'string' || v === '') {
      throw new Error(
        `escurel-test-gateway printed no \`${field}\`: the binary is older than this harness. ` +
          'Rebuild it: cargo build --release -p escurel-test-support --bin escurel-test-gateway',
      );
    }
  }
  return parsed as unknown as GatewayInfo;
}
