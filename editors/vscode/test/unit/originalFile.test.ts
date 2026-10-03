import { describe, expect, it } from 'vitest';
import { originalFileName } from '../../src/commands/originalFile';

// The original of a document page is written to a temp file and opened with the system's own app, so
// the file needs the right extension, and a name that cannot escape the folder it is written to.
describe('originalFileName', () => {
  it('names the file after the page and gives it the extension its content type implies', () => {
    expect(originalFileName('markdown/instances/contract__nda-2026.md', 'application/pdf')).toBe(
      'nda-2026.pdf',
    );
    expect(
      originalFileName(
        'markdown/instances/contract__nda-2026.md',
        'application/vnd.openxmlformats-officedocument.wordprocessingml.document',
      ),
    ).toBe('nda-2026.docx');
    expect(originalFileName('markdown/instances/memo__m1.md', 'text/plain')).toBe('m1.txt');
  });

  it('falls back to .bin for a type it does not know', () => {
    expect(originalFileName('markdown/instances/x__y.md', 'application/x-weird')).toBe('y.bin');
  });

  it('keeps the name a plain file name: no separators, no dots at the start', () => {
    const name = originalFileName('markdown/instances/x__../../etc/passwd.md', 'application/pdf');
    expect(name).not.toContain('/');
    expect(name.startsWith('.')).toBe(false);
    expect(name.endsWith('.pdf')).toBe(true);
  });
});
