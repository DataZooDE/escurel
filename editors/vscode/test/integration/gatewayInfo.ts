/** The one line of JSON `escurel-test-gateway` prints. */
export interface GatewayInfo {
  gateway_url: string;
  issuer_url: string;
  kid: string;
  signing_key: string;
  bearer: string;
  tenant: string;
}
