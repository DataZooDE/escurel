import { LoroDoc } from 'loro-crdt';
import { EscurelError, type Draft, type EscurelClient } from '../client';
import { describeError } from '../errors';
import { log } from '../log';

/**
 * Find an existing open draft for a target page, or undefined if none.
 *
 * SPEC §7 PR-1 / §8 M2: one open draft per page. A draft with any other status
 * (promoted, discarded) is history and cannot accept new edits.
 */
export function findOpenDraftForPage(
  drafts: readonly Draft[],
  targetPageId: string,
): Draft | undefined {
  return drafts.find((d) => d.status === 'open' && d.target_page_id === targetPageId);
}

/**
 * Whether `draft` is the caller's own — to read as work in progress, and to edit.
 *
 * `list_drafts` returns every draft the caller may SEE, an agent's proposal
 * awaiting review included, and showing that in place of the page would present
 * unreviewed content as if it were the record. An absent `subject` is a gateway
 * with no verifier, where every caller is the same principal and the question has
 * one answer.
 */
export function isOwnDraft(draft: Draft, subject?: string): boolean {
  return subject === undefined || draft.author === subject;
}

/** The refusal for a page held by somebody else's draft. */
function heldByAnother(draftId: string): EscurelError {
  return new EscurelError(
    'forbidden',
    `escurel: this page already has an open draft (${draftId}) that is not yours — ` +
      'review or discard it before editing',
  );
}

/**
 * The open draft for `pageId`, if the gateway shows one.
 *
 * Read path twin of [`writeInstanceDraft`]: an instance with work in progress
 * reads as that work rather than as the page it will land on.
 */
export async function draftForPage(
  client: EscurelClient,
  pageId: string,
  subject?: string,
): Promise<Draft | undefined> {
  const draft = findOpenDraftForPage(await client.listDrafts(), pageId);
  return draft && isOwnDraft(draft, subject) ? draft : undefined;
}

/**
 * Replaces the entire body of a Loro document initialized from a session snapshot.
 *
 * The gateway session document must be imported first: local ops constructed
 * from scratch without the session's history are buffered by Loro as unresolved
 * dependencies, causing apply_op to report success while the gateway silently
 * drops the content on commit.
 *
 * A null snapshot indicates live editing is unsupported for this draft. Refuse
 * immediately rather than generating an op that will be swallowed.
 */
export function buildReplaceOpFromSnapshot(
  snapshotBase64: string | null | undefined,
  nextMarkdown: string,
): string {
  if (!snapshotBase64) {
    throw new EscurelError('refused', 'cannot edit live: session provided no snapshot');
  }

  const doc = new LoroDoc();
  doc.import(Buffer.from(snapshotBase64, 'base64'));
  const from = doc.version();

  const text = doc.getText('body');
  if (text.length > 0) {
    text.delete(0, text.length);
  }
  text.insert(0, nextMarkdown);
  doc.commit();

  const update = doc.export({ mode: 'update', from });
  return Buffer.from(update).toString('base64');
}

/**
 * Lands markdown into a personal draft for an instance page (SPEC §8 M2, §7 PR-1).
 *
 * Ensures an open draft exists for the page (reusing an existing one or creating
 * it), opens a live editing session, applies a full-body replacement op built
 * from the session snapshot, and commits the session to write the bytes back.
 */
export async function writeInstanceDraft(
  client: EscurelClient,
  pageId: string,
  content: string,
  baseSha256?: string,
  subject?: string,
): Promise<{ draftId: string; created: boolean }> {
  const drafts = await client.listDrafts();
  const open = findOpenDraftForPage(drafts, pageId);
  // Somebody else's held write blocks the page for both of us: it is not ours to
  // edit, and a second draft against the same page would be refused anyway.
  if (open && !isOwnDraft(open, subject)) throw heldByAnother(open.draft_id);
  const existing = open;
  let draftId: string;

  if (existing) {
    draftId = existing.draft_id;
  } else {
    draftId = await client
      .createDraft({
        target_page_id: pageId,
        content,
        ...(baseSha256 ? { base_sha256: baseSha256 } : {}),
      })
      .then((created) => created.draft.draft_id)
      .catch(async (e: unknown) => {
        // Lost the race: a page carries at most one open draft, and something
        // opened one between the read above and this create. The draft that now
        // exists is the one this save belongs in, so look it up rather than
        // handing the user a conflict about a tool they never called.
        if (e instanceof EscurelError && e.kind === 'conflict') {
          const raced = findOpenDraftForPage(await client.listDrafts(), pageId);
          if (raced && isOwnDraft(raced, subject)) return raced.draft_id;
          if (raced) throw heldByAnother(raced.draft_id);
        }
        throw e;
      });
  }

  const created = existing === undefined;
  // A session is how an EXISTING draft is edited, so losing it loses the edit. A
  // draft just created already holds these exact bytes, though, so a gateway with
  // no live CRDT mode — or one at its session cap — has still saved the user's
  // work: reporting a failed save would be a lie, and would leave the document
  // dirty over content that is already held for review.
  let sessionInfo;
  try {
    sessionInfo = await client.openSession({ draft_id: draftId });
  } catch (e) {
    if (created) {
      log().warn(
        `escurel: held ${pageId} as draft ${draftId} without a live session: ${describeError(e)}`,
      );
      return { draftId, created };
    }
    if (e instanceof EscurelError && e.kind === 'forbidden') throw heldByAnother(draftId);
    throw e;
  }

  // Opening a second session on the same draft returns JSON-RPC -32603 and cannot
  // be recovered until the 30-minute idle TTL expires. If op application fails,
  // discard the session so subsequent saves do not lock the user out.
  let committed = false;
  try {
    const op = buildReplaceOpFromSnapshot(sessionInfo.snapshot, content);
    await client.applyOp({ session: sessionInfo.session, op });
    await client.closeSession({ session: sessionInfo.session, commit: true });
    committed = true;
  } finally {
    if (!committed) {
      await client
        .closeSession({ session: sessionInfo.session, commit: false })
        .catch(() => undefined);
    }
  }

  return { draftId, created };
}
