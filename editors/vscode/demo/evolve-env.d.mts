export interface EvolveEnvInput {
  gatewayUrl: string;
  adminBearer: string;
  issuerUrl: string;
  tenant: string;
}
export function evolveAgentEnv(input: EvolveEnvInput): Record<string, string>;
