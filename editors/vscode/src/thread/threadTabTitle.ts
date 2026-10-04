const MAX = 32;

/** A tab names the thread in a few words; the Threads outline carries the full event title. */
export function threadTabTitle(title: string): string {
  const text = title.trim();
  if (!text) return 'Thread';
  if (text.length <= MAX) return `Thread · ${text}`;
  const cut = text.slice(0, MAX - 1);
  const space = cut.lastIndexOf(' ');
  const word = space > MAX / 2 ? cut.slice(0, space) : cut;
  return `Thread · ${word.replace(/[\s:,;·-]+$/, '')}…`;
}
