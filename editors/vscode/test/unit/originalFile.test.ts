import { describe, expect, it } from 'vitest';
import { planOriginal } from '../../src/commands/originalFile';

const scope = 'https://gw.example/acme';

// The original of a document page is UNTRUSTED bytes someone else uploaded. It is written to the
// extension's storage and, only if its type is passive, handed to the system's own application. Active
// content (HTML, SVG, scripts) must never reach a handler: a `file://` HTML page opens in the browser
// with script enabled.
describe('planOriginal: what is written and what happens with it', () => {
  it('opens a PDF or an image straight away, under a name that is a hash, not the upload', () => {
    const pdf = planOriginal(scope, 'markdown/instances/contract__nda-2026.md', 'application/pdf');
    expect(pdf.handling).toBe('open');
    expect(pdf.fileName).toMatch(/^nda-2026-[0-9a-f]{12}\.pdf$/);
    expect(planOriginal(scope, 'markdown/instances/x__y.md', 'image/png').handling).toBe('open');
    expect(planOriginal(scope, 'markdown/instances/x__y.md', 'image/jpeg').fileName).toMatch(
      /\.jpg$/,
    );
    expect(planOriginal(scope, 'markdown/instances/x__y.md', 'text/plain').handling).toBe('open');
  });

  it('asks first for an office document, because the system app may run what it contains', () => {
    for (const type of [
      'application/vnd.openxmlformats-officedocument.wordprocessingml.document',
      'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet',
      'application/vnd.openxmlformats-officedocument.presentationml.presentation',
    ]) {
      expect(planOriginal(scope, 'markdown/instances/x__y.md', type).handling).toBe('confirm');
    }
  });

  it('never gives active content an extension a handler would run: it is saved as text and only revealed', () => {
    for (const type of [
      'text/html',
      'application/xhtml+xml',
      'image/svg+xml',
      'application/javascript',
      'text/javascript',
      'application/x-msdownload',
      'application/x-weird',
      'text/html; charset=utf-8',
    ]) {
      const plan = planOriginal(scope, 'markdown/instances/x__y.md', type);
      expect(plan.handling, type).toBe('reveal');
      expect(plan.fileName, type).toMatch(/\.(txt|bin)$/);
    }
  });

  it('gives two documents with the same slug in different tenants different files', () => {
    const a = planOriginal('https://gw/acme', 'markdown/instances/c__nda.md', 'application/pdf');
    const b = planOriginal('https://gw/globex', 'markdown/instances/c__nda.md', 'application/pdf');
    expect(a.fileName).not.toBe(b.fileName);
    expect(
      planOriginal('https://gw/acme', 'markdown/instances/c__nda.md', 'application/pdf'),
    ).toEqual(a);
  });

  it('keeps the name a plain file name: no separators, no dots at the start', () => {
    const { fileName } = planOriginal(
      scope,
      'markdown/instances/x__../../etc/passwd.md',
      'application/pdf',
    );
    expect(fileName).not.toContain('/');
    expect(fileName.startsWith('.')).toBe(false);
    expect(fileName.endsWith('.pdf')).toBe(true);
  });
});
