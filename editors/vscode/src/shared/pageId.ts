/**
 * Page ids and the slugs a surface shows instead of them.
 *
 * Corpora name instance pages two ways: flat, `instances/<skill>__<id>.md`
 * (the shipped ones), and nested, `instances/<skill>/<id>.md` (server
 * fixtures). A surface shows the id; which half of a flat name that is
 * depends on whether the skill is known, so both rules live here.
 */

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
  if (skill) return file.startsWith(`${skill}__`) ? file.slice(skill.length + 2) : file;
  const sep = file.indexOf('__');
  return sep >= 0 ? file.slice(sep + 2) : file;
}
