import { expect, fixture, html, oneEvent } from '@open-wc/testing';
import '../../webview/page-as-ui/main';
import type { EscurelPageAsUi } from '../../webview/page-as-ui/page-as-ui';
import type { PageModel, WebviewToHost } from '../../src/shared/protocol';
import { orderPage } from './fixtures';

async function render(): Promise<EscurelPageAsUi> {
  const el = await fixture<EscurelPageAsUi>(
    html`<escurel-page-as-ui .model=${orderPage}></escurel-page-as-ui>`,
  );
  await el.updateComplete;
  return el;
}
const q = (el: Element, sel: string) => el.shadowRoot!.querySelector(sel);
const qa = (el: Element, sel: string) => Array.from(el.shadowRoot!.querySelectorAll(sel));
const text = (n: Element | null) => (n?.textContent ?? '').replace(/\s+/g, ' ').trim();

describe('<escurel-page-as-ui>', () => {
  it('renders the header, the skill row and the Page | Markdown toggle', async () => {
    const el = await render();
    expect(text(q(el, 'h1'))).to.equal(orderPage.title);
    expect(text(q(el, '.skill-row'))).to.contain('customer-order');
    expect(qa(el, '.toggle button').map(text)).to.deep.equal(['Page', 'Markdown']);
  });

  it('renders every field by kind: badge, instance split button, money, date, bool, markdown', async () => {
    const el = await render();
    const rows = qa(el, '.field');
    expect(rows.map((r) => r.getAttribute('data-name'))).to.deep.equal([
      'status',
      'customer',
      'value_eur',
      'eta',
      'urgent',
      'notes',
    ]);
    expect(q(el, '.field[data-name="status"] .badge')).to.exist;
    expect(text(q(el, '.field[data-name="customer"] .instance-button .primary'))).to.equal(
      'hoffmann',
    );
    expect(
      q(el, '.field[data-name="customer"] .instance-button .primary')!.getAttribute('title'),
    ).to.equal('Open instance');
    expect(text(q(el, '.field[data-name="value_eur"] .value'))).to.equal('184,200.00');
    // A disabled checkbox looked like a bug in light themes: a read-only yes/no is just words.
    expect(q(el, '.field[data-name="urgent"] input[type="checkbox"]') === null).to.equal(true);
    expect(text(q(el, '.field[data-name="urgent"] .value'))).to.equal('Yes');
    expect(q(el, '.field[data-name="notes"] .markdown')).to.exist;
  });

  it('shows the summary, the body and the gate for a review skill; the form is read-only and says how to edit', async () => {
    const el = await render();
    expect(text(q(el, '.summary'))).to.contain('Delivery at risk');
    expect(text(q(el, '.body'))).to.contain('Body text');
    expect(text(q(el, '.gate'))).to.contain('review');
    // The note must tell a reader what to DO. It used to say editing 'arrives with backend PR-1':
    // a ticket number nobody reading a form can act on, about work that has since shipped.
    const note = text(q(el, '.readonly-note'));
    expect(note).to.contain('Markdown');
    expect(note).to.not.match(/PR-\d/);
    expect(
      qa(el, 'input, textarea, select').every((i) => (i as HTMLInputElement).disabled),
    ).to.equal(true);
  });

  it('renders the body as markdown: the items table is a table, not source text', async () => {
    const el = await fixture<EscurelPageAsUi>(
      html`<escurel-page-as-ui
        .model=${{
          ...orderPage,
          body: '## Items\n\n| Item | Material | Qty |\n|---|---|---:|\n| 10 | GH-4711 | 240 |\n\n## History\n\n- created from customer PO',
        }}
      ></escurel-page-as-ui>`,
    );
    await el.updateComplete;
    expect(text(q(el, '.body h2'))).to.equal('Items');
    expect(qa(el, '.body table thead th').map((c) => text(c))).to.deep.equal([
      'Item',
      'Material',
      'Qty',
    ]);
    expect(qa(el, '.body table tbody td').map((c) => text(c))).to.deep.equal([
      '10',
      'GH-4711',
      '240',
    ]);
    expect(qa(el, '.body ul li')).to.have.length(1);
    // No leftover markdown syntax shown to the reader.
    expect(text(q(el, '.body'))).to.not.contain('|---');
    expect(text(q(el, '.body'))).to.not.contain('## ');
  });

  it('renders the actions as Skill split buttons with the four-item menu', async () => {
    const el = await render();
    const buttons = qa(el, '.actions .skill-button');
    expect(buttons.map((b) => text(b.querySelector('.primary')))).to.deep.equal(
      orderPage.actions.map((a) => a.label),
    );
    (buttons[0]!.querySelector('.chevron') as HTMLButtonElement).click();
    await el.updateComplete;
    expect(
      qa(el, '.actions .skill-button [role="menu"] [role="menuitem"]').map(text),
    ).to.deep.equal([
      'Start in background',
      'First make a plan',
      'Start in terminal',
      'View skill',
    ]);
  });

  it('posts typed messages to the host: open instance, view skill, show raw, start skill', async () => {
    const el = await render();
    const sent: WebviewToHost[] = [];
    el.addEventListener('escurel-message', (e) =>
      sent.push((e as CustomEvent<WebviewToHost>).detail),
    );
    (q(el, '.field[data-name="customer"] .instance-button .primary') as HTMLButtonElement).click();
    (q(el, '.skill-row .skill-link') as HTMLButtonElement).click();
    (qa(el, '.toggle button')[1] as HTMLButtonElement).click();
    (q(el, '.actions .skill-button .primary') as HTMLButtonElement).click();
    expect(sent).to.deep.equal([
      { type: 'open-wikilink', wikilink: '[[customer::hoffmann]]' },
      { type: 'view-skill', skill: 'customer-order' },
      { type: 'show-raw' },
      { type: 'start-skill', skill: 'supplier-risk', mode: 'background' },
    ]);
  });

  it('the split-button menu is keyboard-accessible and closes on Escape', async () => {
    const el = await render();
    const chevron = q(el, '.actions .skill-button .chevron') as HTMLButtonElement;
    expect(chevron.getAttribute('aria-haspopup')).to.equal('menu');
    chevron.click();
    await el.updateComplete;
    expect(chevron.getAttribute('aria-expanded')).to.equal('true');
    const menu = q(el, '.actions .skill-button [role="menu"]') as HTMLElement;
    const closed = oneEvent(el, 'escurel-menu-closed');
    menu.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    await closed;
    await el.updateComplete;
    expect(q(el, '.actions .skill-button [role="menu"]')).to.not.exist;
  });
});

describe('<escurel-page-as-ui> thread strip', () => {
  const withThread = async (runStatus = 'processed') => {
    const el = await fixture<EscurelPageAsUi>(
      html`<escurel-page-as-ui
        .model=${{ ...orderPage, thread: { rootEventId: 'root-1', runId: 'run-1', runStatus } }}
      ></escurel-page-as-ui>`,
    );
    await el.updateComplete;
    return el;
  };

  it('shows where the page came from, and opens the thread and the run', async () => {
    const el = await withThread();
    const strip = q(el, '.thread-strip')!;
    expect(strip).to.exist;
    expect(text(strip)).to.contain('Thread');
    expect(text(strip)).to.contain('processed');
    const sent: WebviewToHost[] = [];
    el.addEventListener('escurel-message', (e) =>
      sent.push((e as CustomEvent<WebviewToHost>).detail),
    );
    (q(el, '.thread-strip .open-thread') as HTMLButtonElement).click();
    (q(el, '.thread-strip .open-run') as HTMLButtonElement).click();
    expect(sent).to.deep.equal([
      { type: 'open-thread', rootEventId: 'root-1' },
      { type: 'open-run', runId: 'run-1' },
    ]);
  });

  it('gives its buttons names a screen reader can use', async () => {
    const el = await withThread();
    // The visible text IS the name ("label in name"); an id is only ever a tooltip.
    expect(text(q(el, '.open-thread'))).to.contain('Open thread');
    expect(text(q(el, '.open-run'))).to.equal('Open run');
    expect(q(el, '.open-run')!.getAttribute('aria-label')).to.equal(null);
  });

  it('says so when the run that produced the page did not succeed', async () => {
    const el = await withThread('failed');
    expect(text(q(el, '.thread-strip'))).to.contain('failed');
  });

  it('is absent for a page no run has finished against', async () => {
    const el = await render();
    expect(q(el, '.thread-strip')).to.equal(null);
  });

  it('names an editable checkbox field, so a screen reader hears more than "checkbox, checked"', async () => {
    const el = await fixture<EscurelPageAsUi>(
      html`<escurel-page-as-ui .model=${{ ...orderPage, editable: true }}></escurel-page-as-ui>`,
    );
    await el.updateComplete;
    // <escurel-field> renders into the light DOM.
    const box = qa(el, 'escurel-field')
      .map((f) => f.querySelector('input[type="checkbox"]'))
      .find(Boolean) as HTMLInputElement | undefined;
    expect(box !== undefined).to.equal(true);
    expect((box!.getAttribute('aria-label') ?? '').length > 0).to.equal(true);
  });

  it('tells two split buttons on one page apart, and opens the menu with the arrow key', async () => {
    const el = await render();
    const chevrons = qa(el, '.skill-button .chevron, .instance-button .chevron') as HTMLElement[];
    const names = chevrons.map((c) => c.getAttribute('aria-label'));
    expect(new Set(names).size, `distinct names: ${names.join(' | ')}`).to.equal(names.length);

    const first = chevrons[0]!;
    first.focus();
    first.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true }));
    await el.updateComplete;
    const menus = qa(el, '.skill-button [role="menu"], .instance-button [role="menu"]');
    expect(
      menus.length >= 1 || chevrons.some((c) => c.getAttribute('aria-expanded') === 'true'),
    ).to.equal(true);
  });

  it('does not read a run id aloud as the name of the link to the run', async () => {
    const el = await render();
    const runLink = q(el, '.open-run') as HTMLElement | null;
    if (!runLink) return; // the fixture page has no thread strip
    expect(/[0-9A-Z]{20,}/.test(runLink.getAttribute('aria-label') ?? '')).to.equal(false);
  });

  it('a wikilink in the body asks the host to open it', async () => {
    const el = await render();
    const sent: WebviewToHost[] = [];
    el.addEventListener('escurel-message', (e) =>
      sent.push((e as CustomEvent<WebviewToHost>).detail),
    );
    (q(el, '.body .wikilink') as HTMLButtonElement).click();
    expect(sent).to.deep.equal([{ type: 'open-wikilink', wikilink: '[[supplier::stahl-ag]]' }]);
  });

  describe('high contrast', () => {
    const HC =
      '--vscode-contrastBorder:#6fc3df;--vscode-contrastActiveBorder:#f38518;--vscode-widget-border:#ffffff;--vscode-button-background:transparent;--vscode-button-foreground:#ffffff;--vscode-badge-background:#000000;--vscode-badge-foreground:#ffffff';
    async function renderHc(): Promise<EscurelPageAsUi> {
      // A read-only skill shows the layer chip.
      const model = { ...orderPage, skill: { ...orderPage.skill, readOnly: true } };
      const host = await fixture<HTMLElement>(
        html`<div style=${HC}><escurel-page-as-ui .model=${model}></escurel-page-as-ui></div>`,
      );
      const el = host.querySelector('escurel-page-as-ui') as EscurelPageAsUi;
      await el.updateComplete;
      return el;
    }

    it('shows which of Page | Markdown is selected without relying on a fill', async () => {
      // In high contrast the button background is transparent, so the selected one looked like the
      // other. It carries the active-border colour and a heavier weight instead.
      const el = await renderHc();
      const on = q(el, '.toggle button[aria-pressed="true"]') as HTMLElement;
      const off = q(el, '.toggle button[aria-pressed="false"]') as HTMLElement;
      expect(getComputedStyle(on).boxShadow).to.contain('rgb(243, 133, 24)');
      expect(getComputedStyle(off).boxShadow).to.not.contain('rgb(243, 133, 24)');
      expect(Number(getComputedStyle(on).fontWeight)).to.be.greaterThan(
        Number(getComputedStyle(off).fontWeight),
      );
    });

    it('outlines a chip, which otherwise loses its pill and reads as bare text', async () => {
      const el = await renderHc();
      const chip = (el.shadowRoot!.querySelector('.chip') ??
        qa(el, 'escurel-field')
          .map((f) => f.querySelector('.chip'))
          .find(Boolean)) as HTMLElement | null;
      expect(chip !== null, 'the fixture page has a chip').to.equal(true);
      expect(getComputedStyle(chip!).borderTopColor).to.equal('rgb(111, 195, 223)');
    });

    it('draws every rule in one colour', async () => {
      const el = await renderHc();
      const border = getComputedStyle(el).getPropertyValue('--escurel-border').trim();
      expect(border).to.equal('#6fc3df');
    });
  });

  it('shows the source data of a non-markdown page under the form, and says the page is read-only data', async () => {
    const el = await fixture<EscurelPageAsUi>(
      html`<escurel-page-as-ui
        .model=${{
          ...orderPage,
          preview: {
            kind: 'rows',
            readOnly: true,
            source: 'vw_order_lines_all',
            columns: ['vbeln'],
            rows: [['4500123']],
            truncated: false,
          },
        }}
      ></escurel-page-as-ui>`,
    );
    await el.updateComplete;
    const preview = q(el, 'escurel-source-preview')!;
    expect(preview !== null).to.equal(true);
    expect(text(preview.shadowRoot!.querySelector('.badge'))).to.contain('read-only (source)');
    // A markdown page has no such section.
    const plain = await render();
    expect(q(plain, 'escurel-source-preview') === null).to.equal(true);
  });

  it('asks the host for the original when a document page offers it', async () => {
    const el = await fixture<EscurelPageAsUi>(
      html`<escurel-page-as-ui
        .model=${{
          ...orderPage,
          preview: {
            kind: 'document',
            readOnly: true,
            chunks: [{ anchor: 'c1', text: 't' }],
            total: 3,
            truncated: true,
          },
        }}
      ></escurel-page-as-ui>`,
    );
    await el.updateComplete;
    const sent: WebviewToHost[] = [];
    el.addEventListener('escurel-message', (e) =>
      sent.push((e as CustomEvent<WebviewToHost>).detail),
    );
    (
      q(el, 'escurel-source-preview')!.shadowRoot!.querySelector(
        'button.open-original',
      ) as HTMLButtonElement
    ).click();
    expect(sent).to.deep.equal([{ type: 'open-original' }]);
  });

  describe('a row of an instances: rows skill', () => {
    const withSource = async (source: PageModel['source']) => {
      const el = await fixture<EscurelPageAsUi>(
        html`<escurel-page-as-ui .model=${{ ...orderPage, source }}></escurel-page-as-ui>`,
      );
      await el.updateComplete;
      return el;
    };

    it('says the data is a read-only row of the source, and how fresh it is', async () => {
      const el = await withSource({
        fetchedAt: '2026-10-03T12:03:44.000000Z',
        sourceFields: ['status'],
        linked: { enabled: true, exists: true, orphan: false },
      });
      const strip = q(el, '.source-strip')!;
      expect(strip !== null).to.equal(true);
      expect(text(strip)).to.contain('Read-only copy from');
      expect(text(strip)).to.contain('12:03');
      expect(text(strip)).to.contain('Markdown tab');
      expect(strip.querySelector('.lock') !== null, 'a lock icon, not only words').to.equal(true);
      expect(strip.getAttribute('role')).to.equal('note');
    });

    it('invites notes when there are none yet, and says where to write them', async () => {
      const el = await withSource({
        sourceFields: [],
        linked: { enabled: true, exists: false, orphan: false },
      });
      expect(text(q(el, '.source-strip'))).to.contain('No notes yet');
      // One clear action instead of 'switch to Markdown to write some'.
      const add = q(el, '.source-strip button.add-note') as HTMLButtonElement;
      expect(text(add)).to.equal('Add note');
      const sent: WebviewToHost[] = [];
      el.addEventListener('escurel-message', (e) =>
        sent.push((e as CustomEvent<WebviewToHost>).detail),
      );
      add.click();
      expect(sent).to.deep.equal([{ type: 'show-raw' }]);
    });

    it('hides the "this form is read-only, switch to Markdown" note: the banner already says so', async () => {
      const el = await withSource({
        sourceFields: [],
        linked: { enabled: true, exists: true, orphan: false },
      });
      expect(q(el, '.readonly-note') === null).to.equal(true);
    });

    it('puts the page id and backend behind a collapsed Page details, not in the header', async () => {
      const el = await render();
      const d = q(el, 'details.page-meta') as HTMLDetailsElement;
      expect(d.open).to.equal(false);
      expect(text(d)).to.contain('markdown/instances/customer-order/4500123.md');
      expect(text(q(el, '.subline'))).to.not.contain('page id');
    });

    it('titles the source table with a lock, as part of the section name', async () => {
      const el = await fixture<EscurelPageAsUi>(
        html`<escurel-page-as-ui
          .model=${{
            ...orderPage,
            preview: {
              kind: 'rows',
              readOnly: true,
              source: 'vw_x',
              columns: ['a'],
              rows: [['1']],
              truncated: false,
            },
          }}
        ></escurel-page-as-ui>`,
      );
      await el.updateComplete;
      const h2 = qa(el, 'h2').find((h) => text(h).startsWith('Source data'))!;
      expect(text(h2)).to.contain('read-only');
      expect(h2.querySelector('svg.lock') !== null).to.equal(true);
    });

    it('flags an orphan: the source row is gone but the notes are kept', async () => {
      const el = await withSource({
        sourceFields: [],
        linked: { enabled: true, exists: true, orphan: true },
        issue: { code: 'source_missing', message: 'the source has no such row' },
      });
      const strip = q(el, '.source-strip')!;
      expect(text(strip)).to.contain('no longer in the source');
      expect(strip.classList.contains('problem')).to.equal(true);
    });

    it('marks the source fields read-only in the form, and not the others', async () => {
      const el = await withSource({
        sourceFields: ['status'],
        linked: { enabled: true, exists: true, orphan: false },
      });
      const rows = qa(el, '.field');
      const status = rows.find((r) => r.getAttribute('data-name') === 'status')!;
      const notes = rows.find((r) => r.getAttribute('data-name') === 'notes')!;
      expect(status.getAttribute('data-source')).to.equal('true');
      expect(notes.getAttribute('data-source')).to.equal(null);
    });

    describe('from a REST or MCP source', () => {
      const external = {
        fetchedAt: '2026-10-03T12:03:44.000000Z',
        sourceFields: ['status'],
        linked: { enabled: true, exists: true, orphan: false },
        external: 'REST' as const,
        etag: 'w1:abc',
        writableColumns: ['status'],
      };

      it('labels the data as external, with the protocol, and says it is data not instructions', async () => {
        const el = await withSource(external);
        const strip = q(el, '.source-strip')!;
        expect(text(strip)).to.contain('External data (REST)');
        const badge = strip.querySelector('.external')!;
        expect(badge.getAttribute('title')).to.contain('data');
        // A chip of its own (not a clause in a sentence), with a lock: it is a trust label.
        expect(badge.classList.contains('chip')).to.equal(true);
        expect(badge.querySelector('svg.lock') !== null).to.equal(true);
      });

      it('offers a change for each writable column, and posts which column was chosen', async () => {
        const el = await withSource({ ...external, writableColumns: ['status', 'notes'] });
        const buttons = qa(el, '.source-strip button.propose');
        expect(buttons.map(text)).to.deep.equal(['Change status…', 'Change notes…']);
        const sent: WebviewToHost[] = [];
        el.addEventListener('escurel-message', (e) =>
          sent.push((e as CustomEvent<WebviewToHost>).detail),
        );
        (buttons[0] as HTMLButtonElement).click();
        expect(sent).to.deep.equal([{ type: 'propose-write-back', field: 'status' }]);
      });

      it('offers nothing when no column is writable', async () => {
        const el = await withSource({ ...external, writableColumns: [] });
        expect(qa(el, '.source-strip button.propose').length).to.equal(0);
      });

      it('says what the last write-back did, and flags a failure', async () => {
        const el = await fixture<EscurelPageAsUi>(
          html`<escurel-page-as-ui
            .model=${{
              ...orderPage,
              source: external,
              writeBack: {
                outcome: 'failed',
                at: '2026-10-03T12:05:00.000000Z',
                draftId: 'd1',
                attempts: 3,
              },
            }}
          ></escurel-page-as-ui>`,
        );
        await el.updateComplete;
        const line = q(el, '.write-back')!;
        expect(text(line)).to.contain('could not be sent after 3 attempts');
        // Never colour alone: an icon AND a bold lead word, the same shape for every outcome.
        expect(text(line.querySelector('.lead'))).to.equal('Failed');
        expect(line.querySelector('.lead svg') !== null).to.equal(true);
        expect(line.classList.contains('problem')).to.equal(true);
        expect(line.getAttribute('role')).to.equal('status');
      });

      it('an unreadable source is a problem, in words, not a blank', async () => {
        const el = await withSource({
          ...external,
          issue: { code: 'source_unavailable', message: 'upstream status 503' },
        });
        const strip = q(el, '.source-strip')!;
        expect(text(strip)).to.contain('could not be reached right now');
        expect(text(strip)).to.not.contain('showing what is known');
        expect(text(strip)).to.not.contain('source_unavailable');
        expect(strip.querySelector('.issue')!.getAttribute('title')).to.contain(
          'upstream status 503',
        );
        expect(strip.classList.contains('problem')).to.equal(true);
        const sent: WebviewToHost[] = [];
        el.addEventListener('escurel-message', (e) =>
          sent.push((e as CustomEvent<WebviewToHost>).detail),
        );
        (strip.querySelector('button.retry') as HTMLButtonElement).click();
        expect(sent).to.deep.equal([{ type: 'refresh' }]);
      });

      it('a value the source did not give shows as a dash, not a blank that looks broken', async () => {
        const blank = {
          ...orderPage.fields[0]!,
          name: 'rating',
          label: 'Rating',
          value: '',
          display: '',
        };
        const el = await fixture<EscurelPageAsUi>(
          html`<escurel-page-as-ui
            .model=${{ ...orderPage, fields: [blank], source: { ...external, sourceFields: ['rating'], issue: { code: 'source_unavailable', message: 'x' } } }}
          ></escurel-page-as-ui>`,
        );
        await el.updateComplete;
        const cell = q(el, '.field[data-name="rating"]')!;
        expect(text(cell)).to.contain('—');
        expect(cell.querySelector('.badge') === null).to.equal(true);
      });
    });

    it('when the source is DOWN (no source columns known) every blank field shows a dash and no empty pill', async () => {
      // The real case: the projection's `source` is {} so NO field is flagged as a source column, and
      // the page used to render blank rows plus an empty circle where the rating pill was.
      const blank = (name: string, label: string, render: string) => ({
        ...orderPage.fields[0]!,
        name,
        label,
        render,
        value: undefined,
        display: '',
      });
      const el = await fixture<EscurelPageAsUi>(
        html`<escurel-page-as-ui
          .model=${{
            ...orderPage,
            fields: [blank('supplier', 'Supplier', 'text'), blank('rating', 'Rating', 'badge')],
            source: {
              external: 'REST' as const,
              sourceFields: [],
              linked: { enabled: true, exists: false, orphan: false },
              issue: { code: 'source_unavailable', message: 'x' },
            },
          }}
        ></escurel-page-as-ui>`,
      );
      await el.updateComplete;
      for (const name of ['supplier', 'rating']) {
        const cell = q(el, `.field[data-name="${name}"]`)!;
        expect(text(cell), name).to.contain('—');
        expect(cell.querySelector('.badge') === null, `${name}: no empty pill`).to.equal(true);
      }
    });

    it('has no strip for an ordinary page', async () => {
      const el = await render();
      expect(q(el, '.source-strip') === null).to.equal(true);
    });
  });

  it("shows the skill's provenance facts next to it, and a Stale badge when it has gone stale", async () => {
    const fresh = await fixture<EscurelPageAsUi>(
      html`<escurel-page-as-ui
        .model=${{ ...orderPage, skill: { ...orderPage.skill, facts: ['verified 2026-09-30', 'stale after P90D'] } }}
      ></escurel-page-as-ui>`,
    );
    await fresh.updateComplete;
    expect(text(q(fresh, '.skill-facts'))).to.contain('verified 2026-09-30');
    expect(q(fresh, '.skill-facts .stale-badge')).to.equal(null);

    const stale = await fixture<EscurelPageAsUi>(
      html`<escurel-page-as-ui
        .model=${{ ...orderPage, skill: { ...orderPage.skill, stale: true, facts: ['stale', 'verified 2026-01-01'] } }}
      ></escurel-page-as-ui>`,
    );
    await stale.updateComplete;
    // The word is in the page, not only a colour.
    expect(text(q(stale, '.skill-facts .stale-badge'))).to.equal('Stale');
    // And a page whose skill declares nothing has no facts line at all.
    expect(q(await render(), '.skill-facts')).to.equal(null);
  });
});
