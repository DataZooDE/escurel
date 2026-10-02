import { css, html, type TemplateResult } from 'lit';
import { parseMarkdown, type Block, type Inline } from '../../src/shared/markdown';

/**
 * Markdown as elements. The tree comes from `parseMarkdown`, which already refuses everything
 * dangerous (raw HTML is text, images are their alt text, a link needs an http(s) or mailto
 * target), and this only builds Lit templates from it: no `unsafeHTML`, no `innerHTML`, so no
 * path from page content to markup.
 */
export function renderMarkdown(src: string): TemplateResult {
  return html`${parseMarkdown(src).map(block)}`;
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
        // A webview hands a click on an http(s) link to the system browser.
        return html`<a href=${n.href} rel="noopener noreferrer">${inline(n.c)}</a>`;
      case 'wikilink':
        // Inert on purpose: a `[[skill::id]]` needs resolving against the gateway to find its page,
        // and guessing a page id from it would open the wrong page for a nested corpus.
        return html`<span class="wikilink" title=${n.target}>${n.label ?? n.target}</span>`;
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
  .md a:focus-visible {
    outline: 1px solid var(--vscode-focusBorder);
  }
  .md .wikilink {
    color: var(--vscode-textLink-foreground);
    border-bottom: 1px dotted currentColor;
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
