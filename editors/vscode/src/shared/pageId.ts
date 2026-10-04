/**
 * Page ids and the slugs a surface shows instead of them.
 *
 * Corpora name instance pages two ways: flat, `instances/<skill>__<id>.md`
 * (the shipped ones), and nested, `instances/<skill>/<id>.md` (server
 * fixtures). A surface shows the id; which half of a flat name that is
 * depends on whether the skill is known, so both rules live here.
 */

import { cleanText } from './untrustedText';

/** The file name of a page id, without its directories or `.md`. */
export function pageFile(pageId: string): string {
  return pageId.split('/').pop()!.replace(/\.md$/, '');
}

/**
 * The instance id a surface shows. With the skill known, only that exact
 * `<skill>__` prefix is stripped, so a file whose name merely contains `__`
 * keeps it. Without it (a draft row names a target page and nothing else),
 * the first `__` is the separator — the only rule available.
 */
export function pageSlug(pageId: string, skill?: string): string {
  const file = pageFile(pageId);
  // Display text from a page id someone else chose: no bidi or control characters, bounded.
  if (skill) return cleanText(file.startsWith(`${skill}__`) ? file.slice(skill.length + 2) : file, 160);
  const sep = file.indexOf('__');
  return cleanText(sep >= 0 ? file.slice(sep + 2) : file, 160);
}

/**
 * The skill an instance page belongs to, from its id alone: nested `instances/<skill>/<id>.md`, or
 * flat `instances/<skill>__<id>.md`. Anything that is not an instance page (a skill page, a plain file
 * with no skill in its name) has none: a surface must not invent one.
 */
export function pageSkill(pageId: string): string | undefined {
  const match = /(?:^|\/)instances\/(.+)$/.exec(pageId);
  if (!match) return undefined;
  const rest = match[1]!;
  const slash = rest.indexOf('/');
  if (slash > 0) return rest.slice(0, slash);
  const file = rest.replace(/\.md$/, '');
  const sep = file.indexOf('__');
  return sep > 0 ? file.slice(0, sep) : undefined;
}
