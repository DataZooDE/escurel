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
