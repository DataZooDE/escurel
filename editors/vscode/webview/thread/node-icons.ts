import { svg, type TemplateResult } from 'lit';
import type { NodeType } from '../../src/thread/nodeStyle';

/**
 * One small inline icon per card type. Stroke only, drawn in `currentColor`, so a theme (including
 * high contrast) colours them and no icon font has to load. They are decoration: the type is also
 * written on the card, so nothing here carries meaning on its own.
 */
const PATHS: Record<NodeType, TemplateResult> = {
  // A signal: a bolt.
  event: svg`<path d="M9 1.5 3.5 9H8l-1 5.5L12.5 7H8z" />`,
  // A hop that continues somewhere else: an arrow bending down and on.
  cascade: svg`<path d="M3 2.5v4.5a2.5 2.5 0 0 0 2.5 2.5H12" /><path d="m9.5 7 2.5 2.5-2.5 2.5" />`,
  // A run: play inside a ring.
  run: svg`<circle cx="8" cy="8" r="6" /><path d="M6.8 5.6v4.8L10.6 8z" />`,
  // A changeset: two branches meeting, as a pull request does.
  changeset: svg`<circle cx="4.5" cy="3.5" r="1.5" /><circle cx="4.5" cy="12.5" r="1.5" /><circle cx="11.5" cy="12.5" r="1.5" /><path d="M4.5 5v6M11.5 11V6.5A2 2 0 0 0 9.5 4.5H7.5" />`,
  // A page: a sheet with a folded corner.
  page: svg`<path d="M4 1.5h5l3 3v10H4z" /><path d="M9 1.5v3h3M6 8h4M6 10.5h4" />`,
};

export function typeIcon(type: NodeType): TemplateResult {
  return svg`<svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false">${PATHS[type]}</svg>`;
}
