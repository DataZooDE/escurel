// Text from an external source or another person's page is DATA that ends up in tree labels, tooltips
// and notifications. Bidi controls make a name read backwards (`txt.exe` shown as `exe.txt`), control
// characters can garble a UI, and unbounded text floods it.

/** C0/C1 controls, bidi overrides/isolates/marks, zero-width characters and the BOM. */
function isUnsafe(code: number, keepLines: boolean): boolean {
  if (keepLines && (code === 0x09 || code === 0x0a)) return false;
  return (
    code <= 0x1f ||
    (code >= 0x7f && code <= 0x9f) ||
    (code >= 0x200b && code <= 0x200f) ||
    (code >= 0x202a && code <= 0x202e) ||
    (code >= 0x2060 && code <= 0x2064) ||
    (code >= 0x2066 && code <= 0x2069) ||
    code === 0xfeff
  );
}

function strip(text: string, keepLines: boolean): string {
  let out = '';
  for (const ch of text) if (!isUnsafe(ch.codePointAt(0)!, keepLines)) out += ch;
  return out;
}

/** One line of display text: unsafe characters removed, whitespace collapsed, length capped with an ellipsis. */
export function cleanText(text: string, max = 400): string {
  // An 8 MB string is not scanned to keep 400 characters of it.
  if (text.length > max * 8) text = text.slice(0, max * 8);
  // Line breaks and tabs are whitespace to collapse, not characters to drop (`a\nb` is two words).
  const flat = strip(text.replace(/[\t\n\r]+/g, ' '), false)
    .replace(/\s+/g, ' ')
    .trim();
  const chars = [...flat];
  return chars.length <= max ? flat : `${chars.slice(0, max - 1).join('')}…`;
}

/** Multi-line text (a document chunk): line breaks and tabs stay, the rest is stripped, the length capped. */
export function cleanBlock(text: string, max = 4000): string {
  if (text.length > max * 4) text = text.slice(0, max * 4);
  const chars = [...strip(text.replace(/\r\n?/g, '\n'), true)];
  return chars.length <= max ? chars.join('') : chars.slice(0, max).join('');
}

/** Unsafe characters removed and the length capped; spacing is left exactly as it was. */
export function stripUnsafe(text: string, max = 400): string {
  return [...strip(text.length > max * 4 ? text.slice(0, max * 4) : text, false)].slice(0, max).join('');
}

/** `cleanText` for a value that may be absent or not a string. */
export function cleanOpt(value: unknown, max = 400): string | undefined {
  return typeof value === 'string' && value ? cleanText(value, max) || undefined : undefined;
}
