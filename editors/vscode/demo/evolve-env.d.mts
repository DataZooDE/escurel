export interface EvolveEnvInput {
  gatewayUrl: string;
  adminBearer: string;
  issuerUrl: string;
  tenant: string;
  /** Subjects allowed to approve discovered rules; none (the default) means nobody can. */
  ruleApprovers?: string[];
}
export function evolveAgentEnv(input: EvolveEnvInput): Record<string, string>;
