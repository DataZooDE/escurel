import { css, html, type TemplateResult } from 'lit';
import { hostOf, labelClaimsOtherSite } from '../../src/shared/links';
import { chartKey, parseMarkdown, type Block, type Inline } from '../../src/shared/markdown';

/** A chart the host has fetched and checked: a `data:` image and its text description. */
export interface ChartImage {
  src: string;
  alt: string;
}

/**
 * Charts the host resolved for this body, by `chartKey`. The webview draws only what is in here and
 * cannot ask for more: a reference the host did not resolve is shown as its alt text.
 */
export type ChartImages = ReadonlyMap<string, ChartImage>;

// Set for the duration of one (synchronous) render; the templates it builds close over nothing else.
let charts: ChartImages | undefined;

/**
 * Markdown as elements. The tree comes from `parseMarkdown`, which already refuses everything
 * dangerous (raw HTML is text, images are their alt text, a link needs an http(s) or mailto
 * target), and this only builds Lit templates from it: no `unsafeHTML`, no `innerHTML`, so no
 * path from page content to markup.
 */
export function renderMarkdown(src: string, resolved?: ChartImages): TemplateResult {
  charts = resolved;
  try {
    return html`${parseMarkdown(src).map(block)}`;
  } finally {
    charts = undefined;
  }
}

function plainText(nodes: Inline[]): string {
  return nodes
    .map((n) => {
      switch (n.t) {
        case 'text':
        case 'code':
          return n.v;
        case 'wikilink':
          return n.label ?? n.target;
        case 'chart':
          return n.alt;
        default:
          return plainText(n.c);
      }
    })
    .join('');
}

function inline(nodes: Inline[]): TemplateResult[] {
  return nodes.map((n) => {
    switch (n.t) {
      case 'text':
        return html`${n.v}`;
      case 'strong':
        return html`<strong>${inline(n.c)}</strong>`;
      case 'em':
        return html`<em>${inline(n.c)}</em>`;
      case 'code':
        return html`<code>${n.v}</code>`;
      case 'link':
        // A webview hands a click on an http(s) link to the system browser. The tooltip says where it
        // REALLY goes, and a label that reads as a different address is marked.
        return html`<a
          href=${n.href}
          rel="noopener noreferrer"
          class=${labelClaimsOtherSite(plainText(n.c), n.href) ? 'link-mismatch' : ''}
          title=${`Opens ${hostOf(n.href) || n.href}`}
          >${inline(n.c)}</a
        >`;
      case 'chart': {
        const image = charts?.get(chartKey(n.comparison, n.chart));
        // Unresolved (no Evolve, not signed in, failed a check): the description is still there.
        if (!image) return html`${n.alt}`;
        return html`<figure class="chart"><img src=${image.src} alt=${image.alt} /></figure>`;
      }
      case 'wikilink':
        // The webview cannot resolve `[[skill::id]]` (that needs the gateway, and guessing a page id
        // would open the wrong page in a nested corpus), so it says which link was chosen and the
        // host resolves it. A button, so it is reachable and operable from the keyboard.
        return html`<button
          type="button"
          class="wikilink"
          title=${n.target}
          @click=${(e: Event) =>
            (e.currentTarget as HTMLElement).dispatchEvent(
              new CustomEvent<string>('escurel-wikilink', {
                detail: `[[${n.target}]]`,
                bubbles: true,
                composed: true,
              }),
            )}
        >
          ${n.label ?? n.target}
        </button>`;
    }
  });
}

function block(b: Block): TemplateResult {
  switch (b.t) {
    case 'h':
      return heading(b.level, inline(b.c));
    case 'p':
      return html`<p>${inline(b.c)}</p>`;
    case 'ul':
      return html`<ul>
        ${b.items.map((i) => html`<li>${inline(i)}</li>`)}
      </ul>`;
    case 'ol':
      return html`<ol>
        ${b.items.map((i) => html`<li>${inline(i)}</li>`)}
      </ol>`;
    case 'code':
      return html`<pre><code>${b.v}</code></pre>`;
    case 'quote':
      return html`<blockquote>${b.c.map(block)}</blockquote>`;
    case 'hr':
      return html`<hr />`;
    case 'table':
      return html`<div class="table-wrap">
        <table>
          <thead>
            <tr>
              ${b.head.map(
                (c, i) => html`<th class=${cellClass(b.align[i])} scope="col">${inline(c)}</th>`,
              )}
            </tr>
          </thead>
          <tbody>
            ${b.rows.map(
              (row) =>
                html`<tr>
                  ${row.map((c, i) => html`<td class=${cellClass(b.align[i])}>${inline(c)}</td>`)}
                </tr>`,
            )}
          </tbody>
        </table>
      </div>`;
  }
}

function cellClass(align: 'left' | 'right' | 'center' | null | undefined): string {
  return align ? `align-${align}` : '';
}

function heading(level: number, content: TemplateResult[]): TemplateResult {
  switch (level) {
    case 1:
      return html`<h1>${content}</h1>`;
    case 2:
      return html`<h2>${content}</h2>`;
    case 3:
      return html`<h3>${content}</h3>`;
    case 4:
      return html`<h4>${content}</h4>`;
    case 5:
      return html`<h5>${content}</h5>`;
    default:
      return html`<h6>${content}</h6>`;
  }
}

/** Only `--vscode-*` tokens and the shared `--escurel-*` ones; no colour of its own. */
export const markdownStyles = css`
  .md > :first-child {
    margin-top: 0;
  }
  .md > :last-child {
    margin-bottom: 0;
  }
  .md h1,
  .md h2,
  .md h3,
  .md h4,
  .md h5,
  .md h6 {
    font-weight: 600;
    line-height: 1.3;
    margin: 1.1em 0 0.4em;
  }
  .md h1 {
    font-size: 1.35em;
  }
  .md h2 {
    font-size: 1.15em;
  }
  .md h3,
  .md h4,
  .md h5,
  .md h6 {
    font-size: 1em;
  }
  .md p,
  .md ul,
  .md ol,
  .md pre,
  .md blockquote {
    margin: 0.5em 0;
  }
  .md ul,
  .md ol {
    padding-left: 1.6em;
  }
  .md code {
    font-family: var(--vscode-editor-font-family);
    font-size: 0.95em;
    background: var(--vscode-textCodeBlock-background);
    padding: 0 0.3em;
    border-radius: 2px;
  }
  .md pre {
    background: var(--vscode-textCodeBlock-background);
    padding: 8px 12px;
    overflow-x: auto;
  }
  .md pre code {
    background: none;
    padding: 0;
  }
  .md blockquote {
    border-left: 3px solid var(--escurel-border);
    padding-left: 12px;
    color: var(--escurel-muted);
  }
  .md hr {
    border: 0;
    border-top: 1px solid var(--escurel-border);
  }
  .md a {
    color: var(--vscode-textLink-foreground);
  }
  .md a:hover {
    color: var(--vscode-textLink-activeForeground);
  }
  .md a.link-mismatch {
    outline: 1px dashed var(--vscode-editorWarning-foreground);
    outline-offset: 2px;
  }
  .md a:focus-visible {
    outline: 1px solid var(--vscode-focusBorder);
  }
  .md .wikilink {
    all: unset;
    cursor: pointer;
    color: var(--vscode-textLink-foreground);
    border-bottom: 1px dotted currentColor;
  }
  .md .wikilink:hover {
    color: var(--vscode-textLink-activeForeground);
  }
  .md .wikilink:focus-visible {
    outline: 1px solid var(--vscode-focusBorder);
    outline-offset: 1px;
  }
  /* The one colour of its own: Evolve's SVG draws dark text on a transparent background, which is
     unreadable on a dark theme, so the chart sits on a light panel. */
  .md figure.chart {
    margin: 0.6em 0;
  }
  .md figure.chart img {
    display: block;
    max-width: 100%;
    height: auto;
    background: white;
    padding: 6px;
    border: 1px solid var(--escurel-border);
    border-radius: 3px;
  }
  .md .table-wrap {
    overflow-x: auto;
    margin: 0.6em 0;
  }
  .md table {
    border-collapse: collapse;
    min-width: 40%;
  }
  .md th,
  .md td {
    border: 1px solid var(--escurel-border);
    padding: 4px 10px;
    text-align: left;
    vertical-align: top;
  }
  .md th {
    font-weight: 600;
    background: var(--vscode-editorWidget-background);
  }
  .md .align-right {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .md .align-center {
    text-align: center;
  }
`;
