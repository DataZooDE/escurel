import { pageSlug } from '../shared/pageId';

const EXTENSIONS: Record<string, string> = {
  'application/pdf': 'pdf',
  'application/vnd.openxmlformats-officedocument.wordprocessingml.document': 'docx',
  'application/vnd.openxmlformats-officedocument.presentationml.presentation': 'pptx',
  'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet': 'xlsx',
  'text/plain': 'txt',
  'text/markdown': 'md',
  'text/csv': 'csv',
  'text/html': 'html',
  'application/json': 'json',
};

/** A plain file name for a document's original: the page's slug, made safe, with its type's extension. */
export function originalFileName(pageId: string, contentType: string): string {
  const slug = pageSlug(pageId)
    .replace(/[^A-Za-z0-9._-]+/g, '_')
    .replace(/^[._]+/, '')
    .slice(0, 80);
  const ext = EXTENSIONS[contentType.split(';')[0]!.trim().toLowerCase()] ?? 'bin';
  return `${slug || 'document'}.${ext}`;
}
