# examples/chat

The chat corpus for escurel's governed chat agent, as seedable pages. The
same pages ship in code as `escurel_runner_workflow::corpus::chat_corpus()`
(a unit test keeps the two identical).

- `seed/skills/conversation.md` — a private, page-bound conversation: only
  its `owner` (and an admin) can create, read, update or delete it and its chat.
- `seed/skills/chat.md` — the agent's instructions, `autonomy: review`.

```sh
ESCUREL_SEED_DIR=examples/chat/seed escurel-server
```

A question is answered by `escurel-runner` in minted mode; see
`.claude/skills/escurel-platform/references/11-event-driven-agents.md`, *Chat runs*.
