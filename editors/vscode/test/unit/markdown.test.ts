import { describe, expect, it } from 'vitest';
import { parseMarkdown, type Block, type Inline } from '../../src/shared/markdown';

const text = (v: string): Inline => ({ t: 'text', v });
const para = (...c: Inline[]): Block => ({ t: 'p', c });

describe('blocks', () => {
  it('reads headings by level', () => {
    expect(parseMarkdown('# One\n\n### Three')).toEqual([
      { t: 'h', level: 1, c: [text('One')] },
      { t: 'h', level: 3, c: [text('Three')] },
    ]);
  });

  it('joins consecutive lines into one paragraph and splits on a blank line', () => {
    expect(parseMarkdown('a\nb\n\nc')).toEqual([para(text('a b')), para(text('c'))]);
  });

  it('reads bullet and numbered lists', () => {
    expect(parseMarkdown('- one\n- two\n\n1. first\n2. second')).toEqual([
      { t: 'ul', items: [[text('one')], [text('two')]] },
      { t: 'ol', items: [[text('first')], [text('second')]] },
    ]);
  });

  it('reads a fenced code block verbatim, markdown inside it left alone', () => {
    expect(parseMarkdown('```sql\nselect **x**\n```')).toEqual([
      { t: 'code', lang: 'sql', v: 'select **x**' },
    ]);
  });

  it('reads a block quote and a rule', () => {
    expect(parseMarkdown('> note\n\n---')).toEqual([
      { t: 'quote', c: [para(text('note'))] },
      { t: 'hr' },
    ]);
  });
});

describe('tables (the order items the demo shows)', () => {
  const src = [
    '| Item | Material | Qty | Net value |',
    '|---|---|---:|---:|',
    '| 10 | GH-4711 | 240 | 62,400.00 |',
    '| 20 | TH-0815 | 80 | 31,100.00 |',
  ].join('\n');

  it('reads the header, the alignment and the rows', () => {
    const [table] = parseMarkdown(src);
    expect(table).toMatchObject({ t: 'table', align: [null, null, 'right', 'right'] });
    if (table?.t !== 'table') throw new Error('not a table');
    expect(table.head.map((c) => c[0])).toEqual([
      text('Item'),
      text('Material'),
      text('Qty'),
      text('Net value'),
    ]);
    expect(table.rows).toHaveLength(2);
    expect(table.rows[1]!.map((c) => c[0])).toEqual([
      text('20'),
      text('TH-0815'),
      text('80'),
      text('31,100.00'),
    ]);
  });

  it('is not a table without the separator row', () => {
    expect(parseMarkdown('| a | b |\n| c | d |')[0]?.t).toBe('p');
  });

  it('pads a short row and ignores a long one’s extra cells', () => {
    const [table] = parseMarkdown('| a | b |\n|---|---|\n| 1 |\n| 1 | 2 | 3 |');
    if (table?.t !== 'table') throw new Error('not a table');
    expect(table.rows[0]).toHaveLength(2);
    expect(table.rows[1]).toHaveLength(2);
  });
});

describe('inline', () => {
  it('reads strong, emphasis and code', () => {
    expect(parseMarkdown('a **b** *c* `d`')[0]).toEqual(
      para(
        text('a '),
        { t: 'strong', c: [text('b')] },
        text(' '),
        { t: 'em', c: [text('c')] },
        text(' '),
        { t: 'code', v: 'd' },
      ),
    );
  });

  it('keeps an unmatched marker as text rather than eating the line', () => {
    expect(parseMarkdown('2 * 3 and **open')[0]).toEqual(para(text('2 * 3 and **open')));
  });

  it('reads a link with an http(s) or mailto target', () => {
    for (const href of ['https://sap.example/a', 'http://x.example', 'mailto:a@b.example']) {
      expect(parseMarkdown(`[go](${href})`)[0]).toEqual(para({ t: 'link', href, c: [text('go')] }));
    }
  });

  it('reads a wikilink as its own node, with an optional label', () => {
    expect(
      parseMarkdown('see [[supplier::meier-guss]] and [[supplier::meier-guss|the vendor]]')[0],
    ).toEqual(
      para(text('see '), { t: 'wikilink', target: 'supplier::meier-guss' }, text(' and '), {
        t: 'wikilink',
        target: 'supplier::meier-guss',
        label: 'the vendor',
      }),
    );
  });

  it('honours a backslash escape', () => {
    expect(parseMarkdown('a \\*b\\* c')[0]).toEqual(para(text('a *b* c')));
  });
});

describe('what it must never do', () => {
  // The renderer builds elements from this tree and never an HTML string, so the parser's job is to
  // make sure nothing in it can be a script, a handler or a dangerous URL.
  it('does not link a javascript:, data: or vbscript: target: it stays text', () => {
    for (const href of [
      'javascript:alert(1)',
      'JaVaScRiPt:alert(1)',
      'data:text/html,x',
      'vbscript:x',
      ' javascript:x',
    ]) {
      const [p] = parseMarkdown(`[click](${href})`);
      expect(JSON.stringify(p)).not.toContain('"t":"link"');
    }
  });

  it('treats raw HTML as literal text', () => {
    const [p] = parseMarkdown('<script>alert(1)</script> <img src=x onerror=alert(1)>');
    expect(p).toEqual(para(text('<script>alert(1)</script> <img src=x onerror=alert(1)>')));
  });

  it('turns an image into its alt text, never an element that loads a URL', () => {
    expect(parseMarkdown('![logo](https://tracker.example/p.gif)')[0]).toEqual(para(text('logo')));
  });

  it('survives hostile and degenerate input without hanging or throwing', () => {
    const nasty = [
      '*'.repeat(5000),
      '['.repeat(2000),
      '|'.repeat(3000),
      '`'.repeat(4000),
      '> '.repeat(500) + 'x',
    ];
    for (const s of nasty) expect(() => parseMarkdown(s)).not.toThrow();
  });
});

describe('edges that are easy to get wrong', () => {
  it('leaves snake_case and file_names_like_this alone', () => {
    expect(parseMarkdown('sales_doc and delivery_block_flag')[0]).toEqual(
      para(text('sales_doc and delivery_block_flag')),
    );
  });

  it('keeps an unclosed backtick as text', () => {
    expect(parseMarkdown('a ` b')[0]).toEqual(para(text('a ` b')));
  });

  it('reads emphasis inside a link label', () => {
    expect(parseMarkdown('[a **b**](https://x.example)')[0]).toEqual(
      para({
        t: 'link',
        href: 'https://x.example',
        c: [text('a '), { t: 'strong', c: [text('b')] }],
      }),
    );
  });

  it('keeps inline markup in table cells and an escaped pipe in a cell', () => {
    const [table] = parseMarkdown('| a | b |\n|---|---|\n| **x** | y \\| z |');
    if (table?.t !== 'table') throw new Error('not a table');
    expect(table.rows[0]![0]).toEqual([{ t: 'strong', c: [text('x')] }]);
    expect(table.rows[0]![1]).toEqual([text('y | z')]);
  });

  it('ends a paragraph where a list starts', () => {
    expect(parseMarkdown('intro\n- one')).toEqual([
      para(text('intro')),
      { t: 'ul', items: [[text('one')]] },
    ]);
  });
});
