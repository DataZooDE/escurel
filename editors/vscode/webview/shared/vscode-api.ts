import type { WebviewToHost } from '../../src/shared/protocol';

interface VsCodeApi {
  postMessage(message: WebviewToHost): void;
  getState(): unknown;
  setState(state: unknown): void;
}
declare function acquireVsCodeApi(): VsCodeApi;

let api: VsCodeApi | undefined;
/** The webview's one channel to the host; absent outside VS Code (tests, the visual harness). */
export function vscodeApi(): VsCodeApi | undefined {
  if (api) return api;
  try {
    api = acquireVsCodeApi();
  } catch {
    api = undefined;
  }
  return api;
}
