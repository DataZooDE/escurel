/** The thread canvas is a webview panel; VS Code prefixes its view type (`mainThreadWebview-`). */
export function isThreadViewType(viewType: string | undefined): boolean {
  return typeof viewType === 'string' && viewType.endsWith('escurel.thread');
}

/**
 * Whether the Details panel should show its node. It follows the editor in front: a node of a
 * thread is meaningless beside an order page. With no editor at all (focus inside the panel) it
 * keeps what it shows, so clicking into the panel never clears it.
 */
export function shouldShowDetails(f: {
  hasSelection: boolean;
  activeIsThread: boolean;
  activeIsNone: boolean;
}): boolean {
  if (!f.hasSelection) return false;
  return f.activeIsThread || f.activeIsNone;
}
