import type { GatewayClient } from './types';

const TOOL_NAME = /^[a-z][a-z0-9_]{0,63}$/;

function isPlainObject(v: unknown): v is Record<string, unknown> {
  if (typeof v !== 'object' || v === null || Array.isArray(v)) return false;
  const proto = Object.getPrototypeOf(v) as unknown;
  return proto === Object.prototype || proto === null;
}

/**
 * The gateway as another extension may use it. The input is checked here again: the caller is another
 * extension's code and the name ends up in a tool call. The client is looked up per call, so a rebuilt
 * client (another gateway, another identity) is used at once and no token is ever held by the caller.
 */
export function gatewayFacade(
  client: () => { callTool<T>(tool: string, args?: Record<string, unknown>): Promise<T> },
): GatewayClient {
  return {
    callTool<T = unknown>(name: string, args: Record<string, unknown> = {}): Promise<T> {
      if (typeof name !== 'string' || !TOOL_NAME.test(name)) {
        return Promise.reject(new Error('callTool: not a tool name'));
      }
      if (!isPlainObject(args))
        return Promise.reject(new Error('callTool: args must be an object'));
      return client().callTool<T>(name, args);
    },
  };
}
