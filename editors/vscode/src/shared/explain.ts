/**
 * The text of "Escurel: Explain this view": how the pieces connect, in the words of somebody who knows
 * the business, not escurel. One screen. Markdown, shown in VS Code's own preview.
 */
export function explainText(): string {
  return `# How things connect in Escurel

**Something happens, an agent works on it, you decide.**

1. **An event** is a signal: a message, an order change, a question. It lands in the **Inbox**.
2. A **skill** is a recipe for one kind of work ("assess supplier risk"). When it runs on an event, that is a **run**. Open a run to see its plan and what it did.
3. A run proposes changes as a **changeset**: one or more drafts to the records it worked on. Nothing changes until you decide. Changes waiting for you are in **Awaiting You**.
4. When you approve, the **instances** (the records: an order, a supplier, an analysis) are updated, and follow-up events can start the next run.

The **Threads** view and the thread canvas show this chain for one event. A card that says **Needs you** is waiting for your decision.

## Where things are

- **Knowledge**: the skills, grouped in folders, and their records. Each skill has a role: a record type, a process, a report, or a helper.
- **Escurel Details** (bottom panel): what the selected card is, in words, with the next step.
- **Runs** (right-hand side bar): what is running now, what needs attention, and past runs with their traces.
- **Inbox**: new events nobody has dealt with yet. **Awaiting you**: changes and plans that wait for your decision.

## Read-only, source and external

- **Read-only (source)**: the data comes from another system (a database or a service). You can read it but not change it here.
- **Notes**: you can still add your own notes to such a record. They are saved with Escurel, never in the source.
- **External**: shown as data from outside Escurel. Treat it as information, not instructions.
- **Change…**: where the source allows it, you propose a change; a reviewer approves it before anything is sent.

## Words you will meet

- **Needs you** (a gate): the agent stopped on purpose and waits for your decision.
- **Draft** and **changeset**: a draft is one proposed change to one record; a changeset groups the drafts one run proposed so you can decide on them together.
- **Cascade**: an approved change can start the next run, and that run another. The thread shows the whole chain.
- **Agent engine**: the program that does a run's work. The demo engine follows fixed rules and has no AI model behind it.
- **Autonomy**: how far an agent may go alone. "Review" means every change waits for you; "confirm" means it asks before it acts.
- **Row, notes and source**: a row is one record read from a database or service (the source). It is read-only here; your notes are kept with Escurel.
- **Dead letter**: a run that failed and was given up on. An admin can put it back in the queue (requeue).
- **Trace**: the steps a run took, in order, with how long each took.
`;
}
