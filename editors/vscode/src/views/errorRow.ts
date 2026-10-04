/** What a tree's "could not load" row shows and does. Pure; each view maps it onto a TreeItem. */
export interface ErrorRowSpec {
  label: string;
  tooltip: string;
  /** Clicking the row runs this command: the same refresh as the toolbar's. */
  command: string;
}

export function errorRowSpec(message: string, detail?: string): ErrorRowSpec {
  const label = message.trim() || 'Could not load this view.';
  const extra = detail?.trim() ? `\n\n${detail.trim()}` : '';
  return { label, tooltip: `${label}${extra}\n\nClick to try again.`, command: 'escurel.refresh' };
}
