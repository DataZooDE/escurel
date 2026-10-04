// Text from an external source or another person's page is DATA that ends up in tree labels, tooltips
// and notifications. Bidi controls make a name read backwards (`txt.exe` shown as `exe.txt`), control
// characters can garble a UI, and unbounded text floods it.

/** C0/C1 controls except tab/newline (collapsed below), bidi overrides/isolates/marks, zero-width, BOM. */
const STRIP = /[\u0000-\u0008\u000B-\u001F\u007F-\u009F​-‏‪-‮⁠-⁤⁦-⁩﻿]/g;

export function cleanText(text: string, max = 400): string {
  const flat = text.replace(STRIP, '').replace(/\s+/g, ' ').trim();
  const chars = [...flat];
  return chars.length <= max ? flat : `${chars.slice(0, max - 1).join('')}…`;
}

/** Same stripping for multi-line text (a document chunk): line breaks and tabs stay, the length is capped. */
const STRIP_KEEP_LINES =
  /[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F-\u009F\u200B-\u200F\u202A-\u202E\u2060-\u2064\u2066-\u2069\uFEFF]/g;

export function cleanBlock(text: string, max = 4000): string {
  const chars = [...text.replace(/\r\n?/g, '\n').replace(STRIP_KEEP_LINES, '')];
  return chars.length <= max ? chars.join('') : chars.slice(0, max).join('');
}
