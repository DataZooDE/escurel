import { LoroDoc } from 'loro-crdt';
import { EscurelError, type Draft, type EscurelClient } from '../client';

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
 * The open draft for `pageId`, if the gateway shows one.
 *
 * Read path twin of [`writeInstanceDraft`]: an instance with work in progress
 * reads as that work rather than as the page it will land on.
 */
export async function draftForPage(
  client: EscurelClient,
  pageId: string,
): Promise<Draft | undefined> {
  return findOpenDraftForPage(await client.listDrafts(), pageId);
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
): Promise<{ draftId: string; created: boolean }> {
  const drafts = await client.listDrafts();
  const existing = findOpenDraftForPage(drafts, pageId);
  let draftId: string;

  if (existing) {
    draftId = existing.draft_id;
  } else {
    const created = await client.createDraft({
      target_page_id: pageId,
      content,
      ...(baseSha256 ? { base_sha256: baseSha256 } : {}),
    });
    draftId = created.draft.draft_id;
  }

  // A draft is personal, and `list_drafts` shows every draft the caller may SEE —
  // an agent's included. So the draft found above may not be ours to edit, and the
  // page cannot be drafted twice either: the honest answer is that the page is
  // waiting on a decision, not a bare refusal from a tool the user never called.
  const sessionInfo = await client.openSession({ draft_id: draftId }).catch((e: unknown) => {
    if (existing && e instanceof EscurelError && e.kind === 'forbidden') {
      throw new EscurelError(
        'forbidden',
        `escurel: this page already has an open draft (${draftId}) that is not yours — ` +
          'review or discard it before editing',
      );
    }
    throw e;
  });

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

  return { draftId, created: existing === undefined };
}
