import type * as vscode from 'vscode';
import type { WebviewFont, WebviewShellOptions } from './types';

const FAMILY = /^[A-Za-z0-9][A-Za-z0-9 _-]{0,59}$/;
const LANG = /^[A-Za-z]{2,3}(-[A-Za-z0-9]{2,8}){0,3}$/;
const FONT_PATH = /^[A-Za-z0-9._\-/@+~: ]+$/;
const FONT_FILE = /\.(woff2|woff|ttf|otf)$/i;
const FORMAT: Record<string, string> = {
  woff2: 'woff2',
  woff: 'woff',
  ttf: 'truetype',
  otf: 'opentype',
};
const WEIGHTS = new Set([100, 200, 300, 400, 500, 600, 700, 800, 900]);
const MAX_FONTS = 8;
const MAX_TITLE = 200;

export interface ShellExtras {
  /** `<html lang="…">`, or empty. */
  htmlAttrs: string;
  /** Extra CSP directive (with trailing `;`), or empty. */
  fontSrc: string;
  /** `@font-face` rules, or empty. */
  fontCss: string;
  /** `<title>…</title>`, or empty. */
  titleTag: string;
}

const escapeHtml = (s: string): string =>
  s
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');

function fontFace(f: WebviewFont, webview: vscode.Webview, extensionUri: vscode.Uri): string {
  if (typeof f !== 'object' || f === null) throw new Error('webviewHtml: a font must be an object');
  if (typeof f.family !== 'string' || !FAMILY.test(f.family)) {
    throw new Error('webviewHtml: font family is 1 to 60 letters, digits, spaces, "_" or "-"');
  }
  const weight = f.weight ?? 400;
  if (typeof weight !== 'number' || !WEIGHTS.has(weight)) {
    throw new Error('webviewHtml: font weight is 100, 200, ... 900');
  }
  const style = f.style ?? 'normal';
  if (style !== 'normal' && style !== 'italic') {
    throw new Error('webviewHtml: font style is "normal" or "italic"');
  }
  const file = f.file;
  if (typeof file?.path !== 'string' || typeof file.scheme !== 'string') {
    throw new Error('webviewHtml: font file must be a Uri under the extension');
  }
  const root = extensionUri.path.replace(/\/+$/, '');
  const segments = file.path.split('/');
  if (
    file.scheme !== extensionUri.scheme ||
    file.authority !== extensionUri.authority ||
    !file.path.startsWith(`${root}/`) ||
    segments.some((s) => s === '..' || s === '.') ||
    !FONT_PATH.test(file.path) ||
    file.query !== '' ||
    file.fragment !== ''
  ) {
    throw new Error('webviewHtml: font file must lie under the companion extension folder');
  }
  const ext = FONT_FILE.exec(file.path)?.[1]?.toLowerCase();
  if (!ext) throw new Error('webviewHtml: font file must be .woff2, .woff, .ttf or .otf');
  const url = String(webview.asWebviewUri(file));
  if (/["'\\()<>\s]/.test(url)) throw new Error('webviewHtml: font file has an unusable URL');
  return `@font-face{font-family:"${f.family}";src:url("${url}") format("${FORMAT[ext]}");font-weight:${weight};font-style:${style};font-display:swap}`;
}

/** Validates the shell options a companion passes and turns them into markup that cannot escape its slot. */
export function shellExtras(
  o: WebviewShellOptions,
  webview: vscode.Webview,
  extensionUri: vscode.Uri,
): ShellExtras {
  let htmlAttrs = '';
  let fontSrc = '';
  let fontCss = '';
  let titleTag = '';
  if (o.lang !== undefined) {
    if (typeof o.lang !== 'string' || !LANG.test(o.lang)) {
      throw new Error('webviewHtml: lang must be a language tag such as "de" or "pt-BR"');
    }
    htmlAttrs = ` lang="${o.lang}"`;
  }
  if (o.title !== undefined) {
    if (typeof o.title !== 'string' || o.title.length > MAX_TITLE) {
      throw new Error(`webviewHtml: title must be a string of at most ${MAX_TITLE} characters`);
    }
    // eslint-disable-next-line no-control-regex
    const clean = o.title.replace(/[\u0000-\u001f\u007f\u2028\u2029]/g, ' ');
    titleTag = `<title>${escapeHtml(clean)}</title>`;
  }
  if (o.fonts !== undefined) {
    if (!Array.isArray(o.fonts)) throw new Error('webviewHtml: fonts must be a list');
    if (o.fonts.length > MAX_FONTS) throw new Error(`webviewHtml: at most ${MAX_FONTS} fonts`);
    if (o.fonts.length > 0) {
      fontCss = o.fonts.map((f) => fontFace(f, webview, extensionUri)).join('');
      fontSrc = `font-src ${webview.cspSource}; `;
    }
  }
  return { htmlAttrs, fontSrc, fontCss, titleTag };
}
