import * as vscode from 'vscode';

/**
 * The shell every Escurel webview loads: one bundle from `dist/webview/`, a nonce-gated
 * script and nothing else the CSP allows — no network, no inline script, styles only from
 * the webview's own origin. The webview never gets a token or a URL to fetch; it talks to
 * the host over typed postMessage (SPEC §5).
 */
export function webviewHtml(
  webview: vscode.Webview,
  extensionUri: vscode.Uri,
  bundle: string,
  tag: string,
): string {
  const script = webview.asWebviewUri(
    vscode.Uri.joinPath(extensionUri, 'dist', 'webview', `${bundle}.js`),
  );
  const nonce = Array.from({ length: 16 }, () => Math.floor(Math.random() * 36).toString(36)).join(
    '',
  );
  return `<!doctype html><html><head><meta charset="utf-8" />
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src ${webview.cspSource} 'unsafe-inline'; script-src 'nonce-${nonce}';" />
<style>html,body{margin:0;height:100%}</style></head>
<body><${tag}></${tag}><script type="module" nonce="${nonce}" src="${script}"></script></body></html>`;
}

export function webviewOptions(extensionUri: vscode.Uri): vscode.WebviewOptions {
  return {
    enableScripts: true,
    localResourceRoots: [vscode.Uri.joinPath(extensionUri, 'dist', 'webview')],
  };
}
