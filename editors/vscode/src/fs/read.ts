import { stringify } from 'yaml';
import type { EscurelClient } from '../client';

export type PathKind =
  'root' | 'skills-root' | 'instances-root' | 'instances-skill' | 'skill' | 'instance';

/** `escurel:/skills/<id>.md` ↔ `markdown/skills/<id>.md`; `escurel:/instances/<skill>/<id>.md` ↔ `markdown/instances/<skill>/<id>.md`. */
export function pathForPage(pageId: string): string {
  return '/' + pageId.replace(/^markdown\//, '');
}

export function pageIdFromPath(
  path: string,
): { pageId?: string; kind: PathKind; skill?: string } | undefined {
  const parts = path.split('/').filter(Boolean);
  if (parts.length === 0) return { kind: 'root' };
  if (parts[0] === 'skills') {
    if (parts.length === 1) return { pageId: undefined, kind: 'skills-root' };
    if (parts.length === 2 && parts[1]!.endsWith('.md'))
      return { pageId: `markdown/skills/${parts[1]}`, kind: 'skill' };
    return undefined;
  }
  if (parts[0] === 'instances') {
    if (parts.length === 1) return { pageId: undefined, kind: 'instances-root' };
    if (parts.length === 2) return { pageId: undefined, kind: 'instances-skill', skill: parts[1] };
    if (parts.length === 3 && parts[2]!.endsWith('.md'))
      return {
        pageId: `markdown/instances/${parts[1]}/${parts[2]}`,
        kind: 'instance',
        skill: parts[1],
      };
    return undefined;
  }
  return undefined;
}

export interface PageMarkdown {
  text: string;
  /** Hash of the stored bytes — the CAS for the next save. Absent under as_of/scenario or on a very old gateway. */
  sha256?: string;
  /** True when the gateway sent no `content` (pre-#579) and the text is a reassembly of frontmatter + body. */
  degraded: boolean;
  frontmatter: Record<string, unknown>;
  skill: string;
  pageType: string;
  lastWrittenBy?: string | null;
}

/**
 * The one read path behind the `escurel:` filesystem: `expand { raw: true }`.
 * A gateway that predates PR-0 (BACKEND_GAPS) sends no `content`; then the
 * text is reassembled from the parsed frontmatter and body and flagged
 * `degraded`, so a save can warn that formatting may change.
 */
export async function readPageMarkdown(
  client: EscurelClient,
  pageId: string,
): Promise<PageMarkdown | undefined> {
  const e = await client.expand({ page_id: pageId, raw: true });
  if (!e.page) return undefined;
  const degraded = typeof e.content !== 'string';
  const text = degraded ? reassemble(e.frontmatter, e.body) : e.content!;
  return {
    text,
    sha256: e.content_sha256,
    degraded,
    frontmatter: e.frontmatter,
    skill: e.page.skill,
    pageType: e.page.page_type,
    lastWrittenBy: e.page.last_written_by,
  };
}

export function reassemble(frontmatter: Record<string, unknown>, body: string): string {
  const fm = stringify(frontmatter, { lineWidth: 0 }).replace(/\n$/, '');
  return `---\n${fm}\n---\n${body}`;
}
