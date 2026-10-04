const MAX = 32;

/**
 * "Vendor 100234 Meier-Guss: PO 4500087433 confirmed 120 of 200 PC" says WHO first and WHAT after the
 * colon. A tab clips its END, so two threads about one vendor became the same clipped prefix. Lead
 * with what differs: the detail first, the subject after it.
 */
function distinguishing(text: string): string {
  const colon = text.indexOf(': ');
  if (colon < 6 || colon > 40) return text;
  const subject = text.slice(0, colon).trim();
  const detail = text.slice(colon + 2).trim();
  return detail ? `${detail} · ${subject}` : text;
}

/** A tab names the thread in a few words; the Threads outline carries the full event title. */
export function threadTabTitle(title: string): string {
  const text = distinguishing(title.trim());
  if (!text) return 'Thread';
  if (text.length <= MAX) return `Thread · ${text}`;
  const cut = text.slice(0, MAX - 1);
  const space = cut.lastIndexOf(' ');
  const word = space > MAX / 2 ? cut.slice(0, space) : cut;
  return `Thread · ${word.replace(/[\s:,;·-]+$/, '')}…`;
}
