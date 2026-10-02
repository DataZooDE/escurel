/** What `tools/list` says about one tool. The gateway tags every tool with its `scope`. */
export interface ToolInfo {
  name: string;
  scope?: string;
}

/** `unknown` is a real answer: it means "do not decide for the gateway". */
export type AdminState = 'admin' | 'not-admin' | 'unknown';

/**
 * Whether this token is an admin's, read from what the gateway chose to LIST for it.
 *
 * `tools/list` is filtered by role, and every tool carries its scope (`agent` or `admin`), so a
 * caller who sees admin-scoped tools is an admin. There is no role claim to read instead: the
 * extension never decodes a token for a decision.
 *
 * "Not an admin" is only claimed when every listed tool declares a scope and none is admin. An
 * older gateway that does not tag scopes would otherwise look like a gateway full of non-admins,
 * and the extension would deactivate controls the user is entitled to. When in doubt this is
 * `unknown`, controls stay enabled, and the gateway's own refusal is the answer.
 */
export function detectAdminState(tools: readonly ToolInfo[]): AdminState {
  if (tools.some((t) => t.scope === 'admin')) return 'admin';
  if (tools.length > 0 && tools.every((t) => typeof t.scope === 'string')) return 'not-admin';
  return 'unknown';
}
