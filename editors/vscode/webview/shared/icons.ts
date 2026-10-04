import { svg } from 'lit';

/** Small themed icons (currentColor): a shape per meaning, so state never rests on colour alone. */
const frame = (body: ReturnType<typeof svg>, cls: string) =>
  svg`<svg class=${cls} viewBox="0 0 16 16" width="12" height="12" fill="none" stroke="currentColor"
    stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${body}</svg>`;

export const lockIcon = () =>
  frame(
    svg`<rect x="3.5" y="7" width="9" height="6.5" rx="1.2" /><path d="M5.5 7V5a2.5 2.5 0 0 1 5 0v2" />`,
    'lock',
  );
export const checkIcon = () => frame(svg`<path d="M3 8.5l3.2 3L13 4.5" />`, 'check');
export const crossIcon = () => frame(svg`<path d="M4 4l8 8M12 4l-8 8" />`, 'cross');
export const warnIcon = () =>
  frame(svg`<path d="M8 2.5l6 10.5H2z" /><path d="M8 7v3M8 11.6v.1" />`, 'warn');
export const syncIcon = () =>
  frame(
    svg`<path d="M13 8a5 5 0 0 1-8.6 3.4M3 8a5 5 0 0 1 8.6-3.4" /><path d="M11.6 2v2.6H9M4.4 14v-2.6H7" />`,
    'sync',
  );
export const clockIcon = () =>
  frame(svg`<circle cx="8" cy="8" r="5.5" /><path d="M8 5v3.2l2 1.3" />`, 'clock');
