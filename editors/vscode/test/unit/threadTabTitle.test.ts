import { describe, expect, it } from 'vitest';
import { threadTabTitle } from '../../src/thread/threadTabTitle';

// Long event titles made every thread tab a clipped '…guss-2026-10-03.md' prefix. A tab names the
// thread in a few words; the Threads outline holds the full text.
describe('threadTabTitle', () => {
  it('keeps a short title whole', () => {
    expect(threadTabTitle('PO 4500087433 delayed')).toBe('Thread · PO 4500087433 delayed');
  });
  it('cuts a long title at a word and says so', () => {
    const t = threadTabTitle('Vendor 100234 Meier-Guss: PO 4500087433 confirmed 120 of 200 PC');
    expect(t.length <= 'Thread · '.length + 32).toBe(true);
    expect(t.endsWith('…')).toBe(true);
    // The part that tells two threads apart comes FIRST: a tab clips its END.
    expect(t.startsWith('Thread · PO 4500087433 confirmed')).toBe(true);
  });
  it('never returns an empty name', () => {
    expect(threadTabTitle('')).toBe('Thread');
    expect(threadTabTitle('   ')).toBe('Thread');
  });
  it('two threads about one vendor differ within the first words, not after the clip', () => {
    const a = threadTabTitle('Vendor 100234 Meier-Guss: PO 4500087412 confirmation moved +14 days');
    const b = threadTabTitle('Vendor 100234 Meier-Guss: PO 4500087433 confirmed 120 of 200 PC');
    expect(a.slice(0, 22)).not.toBe(b.slice(0, 22));
  });
});
