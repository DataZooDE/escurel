/** `[[skill::id#anchor@version|alias]]` occurrences in markdown, outside fenced code. */
export interface Wikilink {
  text: string;
  start: number;
  end: number;
  skill: string;
  id: string;
  anchor?: string;
  version?: string;
  alias?: string;
}

const LINK =
  /\[\[([A-Za-z0-9_.-]+)::([A-Za-z0-9_./-]+)(?:#([A-Za-z0-9_.-]+))?(?:@([A-Za-z0-9_.-]+))?(?:\|([^\]]+))?\]\]/g;

export function findWikilinks(text: string): Wikilink[] {
  const out: Wikilink[] = [];
  // Mask fenced code so links inside it are not offered.
  const masked = text.replace(/```[\s\S]*?```/g, (m) => ' '.repeat(m.length));
  for (const m of masked.matchAll(LINK)) {
    out.push({
      text: m[0],
      start: m.index!,
      end: m.index! + m[0].length,
      skill: m[1]!,
      id: m[2]!,
      anchor: m[3],
      version: m[4],
      alias: m[5],
    });
  }
  return out;
}
