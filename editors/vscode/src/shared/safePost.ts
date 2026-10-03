import type * as vscode from 'vscode';

/**
 * Post to a webview that may have been closed a moment ago.
 *
 * Two different failures, both meaning "the message was for a view that no longer exists":
 * the `panel.webview` getter THROWS synchronously once the panel is disposed, and
 * `postMessage` on a view that goes away in flight REJECTS. Neither has a waiter, so either one
 * escapes as an unhandled rejection from whatever async path was posting (a live reload, a
 * collapse). Catching only the second left the first, which showed up as an intermittent
 * 'Webview is disposed' in the integration log.
 */
export function safePost(panel: Pick<vscode.WebviewPanel, 'webview'>, message: unknown): void {
  try {
    void panel.webview.postMessage(message).then(undefined, () => undefined);
  } catch {
    // Disposed: nobody to tell.
  }
}
