// What an Escurel editor tab says. VS Code names a custom editor's tab after the last segment of
// its URI ("all.md"), which for a row-backed page means nothing; the host sets the panel title.
import { cleanText } from './untrustedText';

const MAX = 48;

function clip(s: string): string {
  return s.length > MAX ? `${s.slice(0, MAX - 1)}…` : s;
}

/** The page's own title when it says more than the id; otherwise the skill and the id. */
export function pageTabTitle(p: { title: string; skill: string; slug: string }): string {
  const title = cleanText(p.title).trim();
  const slug = cleanText(p.slug).trim();
  const skill = cleanText(p.skill).trim();
  if (title && title !== slug) return clip(title);
  const id = slug || title;
  if (!id) return 'Page';
  return clip(skill ? `${skill} · ${id}` : id);
}

export function skillTabTitle(skillId: string): string {
  const id = cleanText(skillId).trim();
  return id ? clip(`Skill · ${id}`) : 'Skill';
}
