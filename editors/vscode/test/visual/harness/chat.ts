import '../../../webview/chat/chat';
import type { ChatViewState } from '../../../src/shared/protocol';
import type { EscurelChat } from '../../../webview/chat/chat';

// A conversation in each state a person meets: an answer with a cited record and a proposed
// change, a failed answer, a question still being worked on, and two focus chips.
const state: ChatViewState = {
  status: 'ready',
  recent: [
    { key: 'recent:0', title: 'Which customers renew this quarter?', when: '2026-10-25T10:00:00Z' },
    { key: 'recent:1', title: 'Suppliers at risk', when: '2026-10-24T09:00:00Z' },
  ],
  current: { key: 'recent:0', title: 'Which customers renew this quarter?' },
  rows: [
    { kind: 'user', key: 'u-1', text: 'Which customers renew this quarter?', ts: 't1' },
    {
      kind: 'assistant',
      key: 'run:R1:reply',
      text: 'Two customers renew this quarter:\n\n- **Globex**, see [[customer::globex]]\n- **Initech**, see [[customer::initech]]',
      ts: 't2',
      status: 'ok',
      runId: 'R1',
      drafts: [],
    },
    { kind: 'user', key: 'u-2', text: 'Add that Globex extended to 2028.', ts: 't3' },
    {
      kind: 'assistant',
      key: 'run:R2:reply',
      text: 'A draft of [[customer::globex]] is waiting for your review.',
      ts: 't4',
      status: 'ok',
      runId: 'R2',
      drafts: ['D1'],
    },
    { kind: 'user', key: 'u-3', text: 'And Initech?', ts: 't5' },
    {
      kind: 'assistant',
      key: 'run:R3:reply',
      text: "I couldn't finish: the agent run failed.",
      ts: 't6',
      status: 'failed',
      runId: 'R3',
      drafts: [],
    },
    { kind: 'user', key: 'u-4', text: 'Compare these two.', ts: 't7' },
    { kind: 'pending', key: 'pending:u-4', text: 'Working on it: reading 2 records' },
  ],
  focus: [
    { key: 'focus:0', title: 'globex' },
    { key: 'focus:1', title: 'initech' },
  ],
  canSend: true,
};

(document.querySelector('escurel-chat') as EscurelChat).state = state;
