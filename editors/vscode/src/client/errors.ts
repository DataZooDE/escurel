import type { ValidationIssue } from './types';

/**
 * Every refusal the gateway can answer, folded into one typed error. Three
 * channels feed it (see docs/BACKEND_GAPS.md "wire notes"):
 *  - a plain HTTP refusal before any envelope (401/403/429 with `{error}`),
 *  - a JSON-RPC `error` with `data.code`,
 *  - a tool payload `{ok: false, issues[]}` (HTTP 200, `isError: true`).
 */
export type ErrorKind =
  | 'unauthorized'
  | 'forbidden'
  | 'admin_required'
  | 'tenant_suspended'
  | 'quota_exhausted'
  | 'session_cap_reached'
  | 'read_only_replica'
  | 'event_not_found'
  | 'already_assigned'
  | 'unsupported'
  | 'invalid_params'
  | 'not_found'
  | 'conflict'
  | 'already_decided'
  | 'layer_read_only'
  | 'refused'
  | 'rpc'
  | 'transport';

export class EscurelError extends Error {
  readonly kind: ErrorKind;
  readonly retryable: boolean;
  readonly tool?: string;
  /** The payload's issues, for a `refused` / `conflict` / … answer. */
  readonly issues?: ValidationIssue[];
  /** `conflict` on update_page: the head the caller must re-diff against. */
  readonly head_sha256?: string;
  readonly head_content?: string;
  readonly head_version?: string;
  /** `already_decided`: the draft in its final state. */
  readonly draft?: { draft_id: string; status: string; [k: string]: unknown };
  /** The raw payload / error data, for anything not modelled above. */
  readonly data?: unknown;
  readonly httpStatus?: number;

  constructor(
    kind: ErrorKind,
    message: string,
    extra: Partial<Omit<EscurelError, 'kind' | 'message' | 'name'>> = {},
  ) {
    super(message);
    this.name = 'EscurelError';
    this.kind = kind;
    this.retryable = extra.retryable ?? false;
    Object.assign(this, extra);
  }

  /** A tool answered `{ok: false, issues[]}`. */
  static fromPayload(tool: string, payload: Record<string, unknown>): EscurelError {
    const issues = (payload.issues as ValidationIssue[] | undefined) ?? [];
    const code = issues[0]?.code;
    const message = issues[0]?.message ?? `${tool}: refused`;
    const common = { tool, issues, data: payload };
    switch (code) {
      case 'conflict':
        return new EscurelError('conflict', message, {
          ...common,
          head_sha256: payload.head_sha256 as string | undefined,
          head_content: payload.head_content as string | undefined,
          head_version: payload.head_version as string | undefined,
        });
      case 'already_decided':
        return new EscurelError('already_decided', message, {
          ...common,
          draft: payload.draft as EscurelError['draft'],
        });
      case 'forbidden':
      case 'not_found':
      case 'layer_read_only':
      case 'admin_required':
        return new EscurelError(code, message, common);
      default:
        return new EscurelError('refused', message, common);
    }
  }

  /** A JSON-RPC `error` (the SDK's `McpError`): `data.code` when the gateway set one. */
  static fromRpc(tool: string, code: number, message: string, data?: unknown): EscurelError {
    const d = (data ?? {}) as { code?: string; retryable?: boolean };
    const retryable = d.retryable === true;
    const extra = { tool, data, retryable };
    switch (d.code) {
      case 'forbidden':
      case 'admin_required':
      case 'tenant_suspended':
      case 'quota_exhausted':
      case 'session_cap_reached':
      case 'read_only_replica':
      case 'event_not_found':
      case 'already_assigned':
      case 'unsupported':
      case 'layer_read_only':
        return new EscurelError(d.code, message, extra);
      case undefined:
        return new EscurelError(code === -32602 ? 'invalid_params' : 'rpc', message, extra);
      default:
        return new EscurelError('rpc', message, extra);
    }
  }

  /** A plain HTTP refusal (`auth_gate.rs`): no JSON-RPC envelope at all. */
  static fromHttp(
    status: number,
    body: { error?: string; message?: string } | undefined,
  ): EscurelError {
    const message = body?.message ?? `HTTP ${status}`;
    const extra = { httpStatus: status, data: body };
    if (status === 401) return new EscurelError('unauthorized', message, extra);
    if (status === 403)
      return new EscurelError(
        body?.error === 'tenant_suspended' ? 'tenant_suspended' : 'forbidden',
        message,
        extra,
      );
    if (status === 429) {
      const kind =
        body?.error === 'session_cap_reached' ? 'session_cap_reached' : 'quota_exhausted';
      return new EscurelError(kind, message, { ...extra, retryable: true });
    }
    return new EscurelError('transport', message, extra);
  }
}
