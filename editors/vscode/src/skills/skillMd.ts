import { parse, stringify } from 'yaml';

/**
 * An escurel skill page rendered as an Anthropic-style `SKILL.md` (#825) — the
 * editor's twin of the CLI's `escurel skill export` (`crates/escurel-cli/src/skill_md.rs`).
 * Both are tested against the same fixture pair (`crates/escurel-cli/tests/fixtures/skill-md/`),
 * so a rule changed on one side and not the other fails a test. Import stays in the CLI: it
 * goes through `create_draft`, which the editor's review flow already covers.
 *
 * - `id` → `name`, `description` → `description`, the body (the instructions) → the body;
 * - every other key rides in `metadata.escurel`; the page's `skill_md:` keys (SKILL.md keys
 *   escurel does not use) come back out at the top level; the pack importer's `layer` is dropped;
 * - links stay as written, and a fenced section lists each one as a `resolve` hint.
 */
export const SKILL_FILE = 'SKILL.md';

const HINTS_OPEN = '<!-- escurel:resolve-hints -->';
const HINTS_CLOSE = '<!-- /escurel:resolve-hints -->';
const MAPPED = new Set(['kind', 'id', 'description']);
const PLACEMENT = new Set(['layer']);
const PASSTHROUGH = 'skill_md';

export interface ExportedSkill {
  /** The SKILL.md `name` — the skill id, and the folder it is written in. */
  name: string;
  skillMd: string;
}

type Mapping = Record<string, unknown>;

const isMapping = (v: unknown): v is Mapping =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

export function exportSkillMd(page: string): ExportedSkill {
  const { fields, body } = splitFrontmatter(page);
  if (fields.kind !== 'skill') throw new Error('not a skill page: only skills export as SKILL.md');
  const name = fields.id;
  if (typeof name !== 'string' || !name) throw new Error('skill page has no `id:`');
  // The name becomes a folder: the gateway's skill-id rule, minus a leading `.` (`..` is the parent).
  if (!/^[A-Za-z0-9_-][A-Za-z0-9_.-]{0,127}$/.test(name))
    throw new Error(`skill id \`${name}\` cannot name a SKILL.md folder`);
  const description =
    typeof fields.description === 'string'
      ? fields.description
      : typeof fields.summary === 'string'
        ? fields.summary
        : `escurel skill \`${name}\``;

  const escurel: Mapping = {};
  let passthrough: Mapping = {};
  for (const [k, v] of Object.entries(fields)) {
    if (MAPPED.has(k) || PLACEMENT.has(k)) continue;
    if (k === PASSTHROUGH && isMapping(v)) passthrough = { ...v };
    else escurel[k] = v;
  }
  const { metadata: passMeta, ...passRest } = passthrough;
  if (passMeta !== undefined && !isMapping(passMeta))
    throw new Error('`skill_md.metadata` must be a mapping');
  const metadata: Mapping = { ...(passMeta ?? {}) };
  if (Object.keys(escurel).length) metadata.escurel = escurel;

  const out: Mapping = { name, description, ...passRest };
  if (Object.keys(metadata).length) out.metadata = metadata;
  return { name, skillMd: `---\n${stringify(out)}---\n${body}${resolveHints(body)}` };
}

function splitFrontmatter(page: string): { fields: Mapping; body: string } {
  const text = page.replace(/^\uFEFF/, '').replace(/\r\n/g, '\n');
  if (!text.startsWith('---\n')) throw new Error('missing frontmatter');
  const rest = text.slice(4);
  const close = rest.match(/^---$/m);
  if (!close || close.index === undefined) throw new Error('unterminated frontmatter');
  const fields: unknown = parse(rest.slice(0, close.index)) ?? {};
  if (!isMapping(fields)) throw new Error('frontmatter must be a mapping');
  const after = rest.slice(close.index + 3);
  return { fields, body: after.startsWith('\n') ? after.slice(1) : after };
}

/** The appended section: each distinct link once, in document order; empty when there are none. */
function resolveHints(body: string): string {
  const links = [...new Set(linkTargets(body))];
  if (!links.length) return '';
  const items = links.map((l) => `- \`${l}\`\n`).join('');
  return (
    `\n${HINTS_OPEN}\n## escurel references\n\n` +
    'The `[[…]]` links above name pages in an escurel knowledge base. Look one up with the ' +
    'escurel MCP `resolve` tool (`{"wikilink": "[[skill::id]]"}`), then `expand` the page it ' +
    `returns:\n\n${items}${HINTS_CLOSE}\n`
  );
}

/**
 * `[[skill::id#anchor@version]]` for every link outside code, alias dropped — the same reading as
 * `escurel_md::wikilink::parse_wikilinks` (fenced blocks and inline code spans are not links).
 */
function linkTargets(body: string): string[] {
  let inFence = false;
  const lines = body.split(/(?<=\n)/).map((line) => {
    const fence = line.trimStart().startsWith('```');
    const blank = fence || inFence;
    if (fence) inFence = !inFence;
    return blank ? line.replace(/[^\n]/g, ' ') : line;
  });
  const prose = lines.join('').replace(/`[^`\n]*`/g, (m) => ' '.repeat(m.length));
  const out: string[] = [];
  for (const m of prose.matchAll(/\[\[([^[\]\r\n]+?)\]\]/g)) {
    const [target] = splitFirst(m[1]!, '|');
    const [rest, version] = splitFirst(target, '@');
    const [ref, anchor] = splitFirst(rest, '#');
    const sep = ref.indexOf('::');
    const skill = sep >= 0 ? ref.slice(0, sep) : '';
    const id = sep >= 0 ? ref.slice(sep + 2) : ref;
    if (!skill && !id) continue;
    out.push(
      `[[${skill ? skill + '::' : ''}${id}${anchor ? '#' + anchor : ''}${version ? '@' + version : ''}]]`,
    );
  }
  return out;
}

function splitFirst(s: string, sep: string): [string, string | undefined] {
  const i = s.indexOf(sep);
  return i < 0 ? [s.trim(), undefined] : [s.slice(0, i).trim(), s.slice(i + 1).trim()];
}
