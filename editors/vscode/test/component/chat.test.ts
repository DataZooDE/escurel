import { expect, fixture, html } from '@open-wc/testing';
import type { ChatViewState, ChatWebviewToHost } from '../../src/shared/protocol';
import type { EscurelChat } from '../../webview/chat/chat';
import '../../webview/chat/chat';

const ready: ChatViewState = {
  status: 'ready',
  recent: [
    { key: 'recent:0', title: 'Renewals', when: '2026-10-25T10:00:00Z' },
    { key: 'recent:1', title: 'Suppliers', when: '2026-10-24T09:00:00Z' },
  ],
  current: { key: 'recent:0', title: 'Renewals' },
  rows: [
    { kind: 'user', key: 'u-1', text: 'Which customers renew?', ts: 't1' },
    {
      kind: 'assistant',
      key: 'run:R1:reply',
      text: 'Globex renews. See [[customer::globex]].',
      ts: 't2',
      status: 'ok',
      runId: 'R1',
      drafts: [],
    },
    { kind: 'user', key: 'u-2', text: 'And Initech?', ts: 't3' },
    { kind: 'pending', key: 'pending:u-2', text: 'Working on it: searching' },
  ],
  canSend: true,
};

async function render(state: ChatViewState | undefined = ready): Promise<EscurelChat> {
  const el = await fixture<EscurelChat>(html`<escurel-chat .state=${state}></escurel-chat>`);
  await el.updateComplete;
  return el;
}
const q = (el: Element, sel: string) => el.shadowRoot!.querySelector(sel);
const all = (el: Element, sel: string) => [...el.shadowRoot!.querySelectorAll(sel)];
const text = (n: Element | null | undefined) => (n?.textContent ?? '').replace(/\s+/g, ' ').trim();
function sent(el: Element): ChatWebviewToHost[] {
  const out: ChatWebviewToHost[] = [];
  el.addEventListener('escurel-message', (e) =>
    out.push((e as CustomEvent<ChatWebviewToHost>).detail),
  );
  return out;
}

describe('<escurel-chat>', () => {
  it('shows the conversation in order with named turns and a working line', async () => {
    const el = await render();
    expect(text(q(el, 'header .title'))).to.equal('Renewals');
    const turns = all(el, '[role="article"]').map((n) => n.getAttribute('aria-label'));
    expect(turns).to.deep.equal(['Your question', 'Answer', 'Your question']);
    expect(text(q(el, '.pending'))).to.equal('Working on it: searching');
    expect(q(el, '.pending')!.getAttribute('aria-busy')).to.equal('true');
  });

  it('sends on Enter, keeps Shift+Enter for a new line, and clears the box', async () => {
    const el = await render();
    const out = sent(el);
    const box = q(el, 'textarea') as HTMLTextAreaElement;
    box.value = 'What about Initech?';
    box.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', shiftKey: true }));
    expect(out).to.deep.equal([]);
    box.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter' }));
    expect(out).to.deep.equal([{ type: 'send', text: 'What about Initech?' }]);
    expect(box.value).to.equal('');
  });

  it('does not send while a question is on its way', async () => {
    const el = await render({ ...ready, canSend: false });
    const out = sent(el);
    const box = q(el, 'textarea') as HTMLTextAreaElement;
    box.value = 'again';
    box.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter' }));
    expect(out).to.deep.equal([]);
    expect((q(el, 'button.send') as HTMLButtonElement).disabled).to.equal(true);
  });

  it('opens a cited link, the run behind an answer, a recent chat and a new one by key only', async () => {
    const el = await render();
    const out = sent(el);
    (q(el, 'button.wikilink') as HTMLButtonElement).click();
    (q(el, 'button.show-run') as HTMLButtonElement).click();
    (all(el, 'details.recent button')[1] as HTMLButtonElement).click();
    (q(el, 'button.new') as HTMLButtonElement).click();
    expect(out).to.deep.equal([
      { type: 'open-wikilink', wikilink: '[[customer::globex]]' },
      { type: 'show-run', key: 'run:R1:reply' },
      { type: 'open-conversation', key: 'recent:1' },
      { type: 'new-chat' },
    ]);
  });

  it('says in words when an answer failed, not by colour alone', async () => {
    const el = await render({
      ...ready,
      rows: [
        {
          kind: 'assistant',
          key: 'run:R2:reply',
          text: "I couldn't finish: the agent run failed.",
          ts: 't',
          status: 'failed',
          drafts: [],
        },
      ],
    });
    expect(text(q(el, '.state-word'))).to.equal('Failed');
    expect(q(el, '[role="article"]')!.getAttribute('aria-label')).to.equal('Answer (failed)');
  });

  it('explains a view that cannot chat instead of showing a dead prompt', async () => {
    const el = await render({
      status: 'unavailable',
      message: 'Chat is not set up in this workspace.',
      recent: [],
      rows: [],
      canSend: false,
    });
    expect(text(q(el, '.status'))).to.equal('Chat is not set up in this workspace.');
    expect(q(el, 'textarea')).to.equal(null);
  });
});
