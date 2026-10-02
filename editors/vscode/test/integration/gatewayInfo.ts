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
