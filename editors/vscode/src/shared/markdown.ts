/**
 * A small markdown parser for page bodies: headings, paragraphs, lists, fenced code, block quotes,
 * rules and pipe tables, with strong, emphasis, code, links and wikilinks inline.
 *
 * It produces a TREE, and the webview builds elements from it. There is no HTML string anywhere,
 * so there is nothing to sanitise: raw HTML in the source is literal text, an image is its alt
 * text, and a link survives only with an http(s) or mailto target. No `vscode` import, so it is
 * tested without one.
 *
 * Not CommonMark. Deliberately a subset (SPEC §9: render plain markdown tables, defer rich
 * blocks), and every construct it does not recognise stays as the text it was.
 */

export type Inline =
  | { t: 'text'; v: string }
  | { t: 'strong'; c: Inline[] }
  | { t: 'em'; c: Inline[] }
  | { t: 'code'; v: string }
  | { t: 'link'; href: string; c: Inline[] }
  | { t: 'wikilink'; target: string; label?: string };

export type Align = 'left' | 'right' | 'center' | null;

export type Block =
  | { t: 'h'; level: 1 | 2 | 3 | 4 | 5 | 6; c: Inline[] }
  | { t: 'p'; c: Inline[] }
  | { t: 'ul' | 'ol'; items: Inline[][] }
  | { t: 'code'; lang?: string; v: string }
  | { t: 'quote'; c: Block[] }
  | { t: 'hr' }
  | { t: 'table'; align: Align[]; head: Inline[][]; rows: Inline[][][] };

const MAX_QUOTE_DEPTH = 6;

export function parseMarkdown(src: string): Block[] {
  return parseBlocks(src.replace(/\r\n?/g, '\n').split('\n'), 0);
}

// ── blocks ───────────────────────────────────────────────────────────────

const HEADING = /^ {0,3}(#{1,6})[ \t]+(.*?)[ \t]*#*[ \t]*$/;
const HR = /^ {0,3}([-*_])(?:[ \t]*\1){2,}[ \t]*$/;
const FENCE = /^ {0,3}(`{3,}|~{3,})[ \t]*([\w+-]*)[ \t]*$/;
const LIST_ITEM = /^( {0,3})([-*+]|\d{1,9}[.)])[ \t]+(.*)$/;
const QUOTE = /^ {0,3}>[ \t]?(.*)$/;
const TABLE_SEPARATOR = /^[ \t]*\|?[ \t]*:?-+:?[ \t]*(?:\|[ \t]*:?-+:?[ \t]*)*\|?[ \t]*$/;

function startsBlock(line: string): boolean {
  return (
    HEADING.test(line) ||
    HR.test(line) ||
    FENCE.test(line) ||
    QUOTE.test(line) ||
    LIST_ITEM.test(line)
  );
}

function parseBlocks(lines: string[], depth: number): Block[] {
  const out: Block[] = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i]!;
    if (line.trim() === '') {
      i += 1;
      continue;
    }

    const fence = FENCE.exec(line);
    if (fence) {
      const marker = fence[1]!;
      const body: string[] = [];
      i += 1;
      while (i < lines.length && !lines[i]!.trim().startsWith(marker)) {
        body.push(lines[i]!);
        i += 1;
      }
      i += 1; // the closing fence, or past the end
      out.push({ t: 'code', ...(fence[2] ? { lang: fence[2] } : {}), v: body.join('\n') });
      continue;
    }

    const heading = HEADING.exec(line);
    if (heading) {
      out.push({
        t: 'h',
        level: heading[1]!.length as 1 | 2 | 3 | 4 | 5 | 6,
        c: parseInline(heading[2]!),
      });
      i += 1;
      continue;
    }

    if (HR.test(line)) {
      out.push({ t: 'hr' });
      i += 1;
      continue;
    }

    if (line.includes('|') && i + 1 < lines.length && TABLE_SEPARATOR.test(lines[i + 1]!)) {
      const head = splitRow(line);
      const sep = splitRow(lines[i + 1]!);
      if (head.length === sep.length && head.length > 0) {
        const rows: Inline[][][] = [];
        i += 2;
        while (i < lines.length && lines[i]!.includes('|') && lines[i]!.trim() !== '') {
          rows.push(fit(splitRow(lines[i]!), head.length).map(parseInline));
          i += 1;
        }
        out.push({
          t: 'table',
          align: sep.map(alignOf),
          head: head.map(parseInline),
          rows,
        });
        continue;
      }
    }

    if (QUOTE.test(line)) {
      const inner: string[] = [];
      while (i < lines.length && QUOTE.test(lines[i]!)) {
        inner.push(QUOTE.exec(lines[i]!)![1]!);
        i += 1;
      }
      // Hostile nesting ("> > > > ...") is read as text past a depth, not recursed into for ever.
      out.push(
        depth >= MAX_QUOTE_DEPTH
          ? { t: 'p', c: [{ t: 'text', v: inner.join(' ') }] }
          : { t: 'quote', c: parseBlocks(inner, depth + 1) },
      );
      continue;
    }

    const item = LIST_ITEM.exec(line);
    if (item) {
      const ordered = /\d/.test(item[2]!);
      const items: string[] = [];
      while (i < lines.length) {
        const m = LIST_ITEM.exec(lines[i]!);
        if (m && /\d/.test(m[2]!) === ordered) {
          items.push(m[3]!);
          i += 1;
        } else if (m || lines[i]!.trim() === '' || !/^\s+\S/.test(lines[i]!)) {
          break;
        } else {
          items[items.length - 1] += ' ' + lines[i]!.trim(); // an indented continuation line
          i += 1;
        }
      }
      out.push({ t: ordered ? 'ol' : 'ul', items: items.map(parseInline) });
      continue;
    }

    const para: string[] = [line.trim()];
    i += 1;
    while (i < lines.length && lines[i]!.trim() !== '' && !startsBlock(lines[i]!)) {
      para.push(lines[i]!.trim());
      i += 1;
    }
    out.push({ t: 'p', c: parseInline(para.join(' ')) });
  }
  return out;
}

/** Cells of a pipe row, without the outer pipes; `\|` is a literal pipe. */
function splitRow(line: string): string[] {
  const cells: string[] = [];
  let cur = '';
  for (let i = 0; i < line.length; i += 1) {
    const ch = line[i]!;
    if (ch === '\\' && line[i + 1] === '|') {
      cur += '|';
      i += 1;
    } else if (ch === '|') {
      cells.push(cur);
      cur = '';
    } else {
      cur += ch;
    }
  }
  cells.push(cur);
  if (cells.length > 0 && cells[0]!.trim() === '') cells.shift();
  if (cells.length > 0 && cells[cells.length - 1]!.trim() === '') cells.pop();
  return cells.map((c) => c.trim());
}

function fit(cells: string[], n: number): string[] {
  const out = cells.slice(0, n);
  while (out.length < n) out.push('');
  return out;
}

function alignOf(sep: string): Align {
  const left = sep.startsWith(':');
  const right = sep.endsWith(':');
  if (left && right) return 'center';
  if (right) return 'right';
  if (left) return 'left';
  return null;
}

// ── inline ───────────────────────────────────────────────────────────────

const SAFE_SCHEME = /^(?:https?:\/\/|mailto:)\S+$/i;

/** A link survives only with an http(s) or mailto target and no control characters in it. */
function safeHref(href: string): boolean {
  if (!SAFE_SCHEME.test(href)) return false;
  for (let i = 0; i < href.length; i += 1) if (href.charCodeAt(i) < 0x20) return false;
  return true;
}

function push(out: Inline[], node: Inline): void {
  const last = out[out.length - 1];
  if (node.t === 'text' && last?.t === 'text') last.v += node.v;
  else out.push(node);
}

export function parseInline(src: string): Inline[] {
  const out: Inline[] = [];
  let i = 0;
  const text = (v: string) => push(out, { t: 'text', v });

  while (i < src.length) {
    const ch = src[i]!;

    if (ch === '\\' && i + 1 < src.length && /[\\`*_[\]()#+\-.!|>~]/.test(src[i + 1]!)) {
      text(src[i + 1]!);
      i += 2;
      continue;
    }

    if (ch === '`') {
      let run = 1;
      while (src[i + run] === '`') run += 1;
      const ticks = '`'.repeat(run);
      const end = src.indexOf(ticks, i + run);
      if (end !== -1) {
        push(out, { t: 'code', v: src.slice(i + run, end).trim() });
        i = end + run;
        continue;
      }
      text(ticks);
      i += run;
      continue;
    }

    if (ch === '[' && src[i + 1] === '[') {
      const end = src.indexOf(']]', i + 2);
      if (end !== -1) {
        const inner = src.slice(i + 2, end);
        if (inner.trim() !== '' && !inner.includes('\n') && !inner.includes('[')) {
          const bar = inner.indexOf('|');
          const target = (bar === -1 ? inner : inner.slice(0, bar)).trim();
          const label = bar === -1 ? undefined : inner.slice(bar + 1).trim();
          if (target) {
            push(out, { t: 'wikilink', target, ...(label ? { label } : {}) });
            i = end + 2;
            continue;
          }
        }
      }
    }

    if ((ch === '[' || (ch === '!' && src[i + 1] === '[')) && !(ch === '[' && src[i + 1] === '[')) {
      const image = ch === '!';
      const open = image ? i + 1 : i;
      const close = matchBracket(src, open);
      if (close !== -1 && src[close + 1] === '(') {
        const paren = src.indexOf(')', close + 2);
        if (paren !== -1) {
          const label = src.slice(open + 1, close);
          const href =
            src
              .slice(close + 2, paren)
              .trim()
              .split(/\s+/)[0] ?? '';
          if (image) {
            // An image would load a URL on its own; show what it says instead.
            text(label);
            i = paren + 1;
            continue;
          }
          if (safeHref(href)) {
            push(out, { t: 'link', href, c: parseInline(label) });
            i = paren + 1;
            continue;
          }
        }
      }
    }

    if ((ch === '*' || ch === '_') && src[i + 1] === ch) {
      const end = src.indexOf(ch + ch, i + 2);
      if (end > i + 2) {
        push(out, { t: 'strong', c: parseInline(src.slice(i + 2, end)) });
        i = end + 2;
        continue;
      }
    }

    if (ch === '*' || ch === '_') {
      const prev = src[i - 1];
      const boundary = ch === '*' || prev === undefined || /[\s([{>,.;:!?"'-]/.test(prev);
      if (boundary && src[i + 1] !== undefined && !/\s/.test(src[i + 1]!)) {
        let end = src.indexOf(ch, i + 1);
        while (end !== -1 && (src[end + 1] === ch || /\s/.test(src[end - 1]!))) {
          end = src[end + 1] === ch ? src.indexOf(ch, end + 2) : src.indexOf(ch, end + 1);
        }
        if (end > i + 1) {
          const next = src[end + 1];
          if (ch === '*' || next === undefined || /[\s)\]}<,.;:!?"'-]/.test(next)) {
            push(out, { t: 'em', c: parseInline(src.slice(i + 1, end)) });
            i = end + 1;
            continue;
          }
        }
      }
    }

    text(ch);
    i += 1;
  }
  return out;
}

/** The `]` closing the `[` at `open`, counting nesting; -1 if there is none. */
function matchBracket(src: string, open: number): number {
  let depth = 0;
  for (let i = open; i < src.length; i += 1) {
    const ch = src[i];
    if (ch === '\\') i += 1;
    else if (ch === '[') depth += 1;
    else if (ch === ']') {
      depth -= 1;
      if (depth === 0) return i;
    }
  }
  return -1;
}
