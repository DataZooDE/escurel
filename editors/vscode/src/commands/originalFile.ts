import { createHash } from 'node:crypto';
import { pageSlug } from '../shared/pageId';

/** What `escurel.openOriginal` does with the bytes once they are written. */
export type OriginalHandling =
  /** Hand the file to the system's own application. */
  | 'open'
  /** Ask the person first: the application may run what the file contains. */
  | 'confirm'
  /** Do NOT open it: show the file in the file manager only. */
  | 'reveal';

export interface OriginalPlan {
  fileName: string;
  handling: OriginalHandling;
}

// The bytes are an UPLOAD by someone else: untrusted. Only passive types are ever given an extension a
// handler would act on; everything else becomes `.txt` (known text-like) or `.bin` and is only revealed.
// HTML and SVG in particular open in the browser from a file:// URL with script enabled.
const OPEN: Record<string, string> = {
  'application/pdf': 'pdf',
  'image/png': 'png',
  'image/jpeg': 'jpg',
  'text/plain': 'txt',
};
const CONFIRM: Record<string, string> = {
  'application/vnd.openxmlformats-officedocument.wordprocessingml.document': 'docx',
  'application/vnd.openxmlformats-officedocument.presentationml.presentation': 'pptx',
  'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet': 'xlsx',
};
/** Text-like types that are safe as `.txt` but are never opened with a handler. */
const AS_TEXT = new Set([
  'text/markdown',
  'text/csv',
  'application/json',
  'text/html',
  'application/xhtml+xml',
]);

/**
 * The file name and handling for a document's original. The name carries a hash of the SCOPE (the gateway
 * and tenant) and the page id, so two documents with the same slug never overwrite each other and the
 * name is not chosen by whoever uploaded the file.
 */
export function planOriginal(scope: string, pageId: string, contentType: string): OriginalPlan {
  const type = contentType.split(';')[0]!.trim().toLowerCase();
  const slug =
    pageSlug(pageId)
      .replace(/[^A-Za-z0-9._-]+/g, '_')
      .replace(/^[._]+/, '')
      .slice(0, 60) || 'document';
  const hash = createHash('sha256').update(`${scope}\n${pageId}`).digest('hex').slice(0, 12);
  const name = (ext: string) => `${slug}-${hash}.${ext}`;
  if (OPEN[type]) return { fileName: name(OPEN[type]), handling: 'open' };
  if (CONFIRM[type]) return { fileName: name(CONFIRM[type]), handling: 'confirm' };
  return { fileName: name(AS_TEXT.has(type) ? 'txt' : 'bin'), handling: 'reveal' };
}
