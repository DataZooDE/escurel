/** Shortens an identifier by cutting its middle: `01M41QQ…X3FFK0`. Never longer than `max`. */
export function middleTruncate(text: string, max: number): string {
  const chars = Array.from(text);
  if (chars.length <= max) return text;
  if (max <= 0) return '';
  if (max === 1) return '…';
  const keep = max - 1;
  const head = Math.ceil(keep / 2);
  const tail = keep - head;
  return `${chars.slice(0, head).join('')}…${tail > 0 ? chars.slice(-tail).join('') : ''}`;
}
