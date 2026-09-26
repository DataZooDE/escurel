// SPEC §8 M2 "done when": an agent changeset of 3 drafts can be reviewed,
// commented, promoted in one action, and the Awaiting count updates live.
import * as assert from 'node:assert/strict';
import * as vscode from 'vscode';
import type { Draft } from '../../../src/client';
import type { EscurelApi } from '../../../src/extension';
import {
  buildReviewCommentThreads,
  extractReviewComments,
  interpretPromoteChangesetResult,
} from '../../../src/review';
import { decodeReviewUri } from '../../../src/review/uri';
import type { ChangesetRow } from '../../../src/views/awaitingModel';
import type { InboxRow } from '../../../src/views/inboxModel';

const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));

async function until<T>(f: () => Promise<T | undefined> | T | undefined, ms = 15_000): Promise<T> {
  const end = Date.now() + ms;
  for (;;) {
    const v = await f();
    if (v !== undefined && v !== null && !(Array.isArray(v) && v.length === 0)) return v;
    if (Date.now() > end) throw new Error('timed out');
    await wait(200);
  }
}

suite('M2', () => {
  let api: EscurelApi;

  // Tracked so suite cleanup can restore modified pages back to original seed state.
  interface SeededTarget {
    pageId: string;
    originalContent: string;
    draftContent: string;
  }
  const targets: SeededTarget[] = [];
  const drafts: Draft[] = [];
  let changesetId: string | undefined;

  suiteSetup(async () => {
    const ext = vscode.extensions.getExtension('datazoo.escurel')!;
    api = (await ext.activate()) as EscurelApi;
    assert.equal(
      vscode.workspace.getConfiguration('escurel').get('gatewayUrl'),
      process.env.ESCUREL_TEST_GATEWAY,
    );
  });

  suiteTeardown(async () => {
    await vscode.commands.executeCommand('workbench.action.closeAllEditors');
    // Restore pages modified by draft promotion so subsequent suites remain independent.
    for (const target of targets) {
      try {
        const cur = await api.services.client.expand({ page_id: target.pageId, raw: true });
        if (cur.content !== target.originalContent && cur.content_sha256) {
          await api.services.client.updatePage({
            page_id: target.pageId,
            content: target.originalContent,
            base_sha256: cur.content_sha256,
          });
        }
      } catch {
        // Best-effort cleanup; unblock runner if gateway state cannot be restored.
      }
    }
  });

  // Helper ensuring 3 drafts in 1 changeset are created exactly once across tests.
  async function ensureThreeDraftChangeset(): Promise<{
    changesetId: string;
    drafts: Draft[];
    targets: SeededTarget[];
  }> {
    if (changesetId && drafts.length === 3) {
      return { changesetId, drafts, targets };
    }

    // Three distinct instances from the seeded corpus so drafts target separate pages.
    const page = await api.services.client.listInstancesPage({ skill_id: 'contact', limit: 3 });
    assert.equal(page.instances.length, 3, 'seeded corpus must contain at least 3 contact pages');

    for (let i = 0; i < page.instances.length; i++) {
      const inst = page.instances[i]!;
      const exp = await api.services.client.expand({ page_id: inst.page_id, raw: true });
      assert.ok(exp.content !== undefined && exp.content_sha256 !== undefined);

      const draftContent = `${exp.content}\n\nProposed revision #${i + 1} for M2 integration.\n`;
      targets.push({
        pageId: inst.page_id,
        originalContent: exp.content,
        draftContent,
      });

      const res = await api.services.client.createDraft({
        target_page_id: inst.page_id,
        content: draftContent,
        base_sha256: exp.content_sha256,
        ...(i === 0 ? { new_changeset: true } : { changeset_id: changesetId }),
      });

      if (i === 0) {
        changesetId = res.draft.changeset_id ?? undefined;
        assert.ok(changesetId, 'first draft must initialize changeset_id');
      }
      drafts.push(res.draft);
    }

    return { changesetId: changesetId!, drafts, targets };
  }

  test('Inbox lists the seeded events, newest first, carrying target page when present', async () => {
    // Wait for the inbox tree to populate from seeded events.
    const nodes = await until(async () => {
      const r = await api.inbox.getChildren();
      return r.length > 0 ? r : undefined;
    });

    const eventRows = nodes.filter((n): n is InboxRow => n.kind === 'event');
    assert.equal(eventRows.length, nodes.length, 'all inbox nodes must be event rows');

    // Events must sort chronologically descending, breaking ties deterministically by event_id.
    for (let i = 0; i < eventRows.length - 1; i++) {
      const cur = eventRows[i]!;
      const next = eventRows[i + 1]!;
      const timeCur = cur.event.at ? new Date(cur.event.at).getTime() : 0;
      const timeNext = next.event.at ? new Date(next.event.at).getTime() : 0;
      if (timeCur !== timeNext) {
        assert.ok(timeCur >= timeNext, `row ${i} must be newer than row ${i + 1}`);
      } else {
        assert.ok(
          next.event.event_id.localeCompare(cur.event.event_id) <= 0,
          'equal timestamps must break ties by event_id descending',
        );
      }
    }

    // A row for an event with an instance_page_id must expose pageId for navigation.
    const targeted = eventRows.find((r) => r.event.instance_page_id);
    assert.ok(targeted, 'seeded corpus must include at least one event targeting an instance');
    assert.equal(targeted.pageId, targeted.event.instance_page_id);
  });

  test('A changeset of three drafts appears in Awaiting you', async () => {
    const { changesetId: csId } = await ensureThreeDraftChangeset();

    // Trigger tree reload to pull the newly drafted changeset from the gateway.
    api.awaiting.refresh();
    const rows = await until(async () => {
      const items = await api.awaiting.getChildren();
      return items.some((i) => i.kind === 'changeset' && i.id === csId) ? items : undefined;
    });

    const csRows = rows.filter((r): r is ChangesetRow => r.kind === 'changeset' && r.id === csId);
    assert.equal(csRows.length, 1, 'must display one changeset row grouping the three drafts');
    assert.ok(
      csRows[0]!.description.includes('3 drafts'),
      `description must count drafts, got: ${csRows[0]!.description}`,
    );

    // TreeView badge reflects the total count of awaiting queue entries.
    assert.equal(api.awaiting.badge?.value, rows.length);
  });

  test('The review diff opens with base and proposed sides in escurel-review scheme', async () => {
    const { drafts: draftList, targets: targetList } = await ensureThreeDraftChangeset();
    const draft = draftList[0]!;
    const target = targetList[0]!;

    // Review entry point routes draft objects to vscode.diff with escurel-review URIs.
    await vscode.commands.executeCommand('escurel.openReview', draft);

    const tab = await until(() => {
      const active = vscode.window.tabGroups.activeTabGroup.activeTab;
      return active?.input instanceof vscode.TabInputTextDiff ? active : undefined;
    });
    const diffInput = tab.input as vscode.TabInputTextDiff;

    // Verify both sides use virtual review URIs rather than touching the local filesystem.
    assert.equal(diffInput.original.scheme, 'escurel-review');
    assert.equal(diffInput.modified.scheme, 'escurel-review');

    const originalDecoded = decodeReviewUri(diffInput.original);
    const modifiedDecoded = decodeReviewUri(diffInput.modified);
    assert.ok(originalDecoded);
    assert.ok(modifiedDecoded);
    assert.equal(originalDecoded.draftId, draft.draft_id);
    assert.equal(originalDecoded.side, 'base');
    assert.equal(modifiedDecoded.draftId, draft.draft_id);
    assert.equal(modifiedDecoded.side, 'proposed');

    // Document provider yields the staged draft content and base instance markdown.
    const [baseDoc, proposedDoc] = await Promise.all([
      vscode.workspace.openTextDocument(diffInput.original),
      vscode.workspace.openTextDocument(diffInput.modified),
    ]);
    assert.equal(proposedDoc.getText(), draft.content);
    assert.equal(baseDoc.getText(), target.originalContent);

    await vscode.commands.executeCommand('workbench.action.closeAllEditors');
  });

  test('Comments round-trip: captured review comment is read back for the draft and isolated from others', async () => {
    const { drafts: draftList } = await ensureThreeDraftChangeset();
    const draft1 = draftList[0]!;
    const draft2 = draftList[1]!;

    const commentBody1 = 'Integration test comment on draft 1';
    const commentLine1 = 3;
    await api.services.client.captureEvent({
      label_skill: 'escurel:review-comment',
      mime: 'text/plain',
      source: 'workbench',
      body: commentBody1,
      provenance: {
        review: {
          draft_id: draft1.draft_id,
          line: commentLine1,
        },
      },
    });

    const commentBody2 = 'Integration test comment on draft 2';
    const commentLine2 = 5;
    await api.services.client.captureEvent({
      label_skill: 'escurel:review-comment',
      mime: 'text/plain',
      source: 'workbench',
      body: commentBody2,
      provenance: {
        review: {
          draft_id: draft2.draft_id,
          line: commentLine2,
        },
      },
    });

    // Gateway stores review comments as system events on the target instance page.
    const eventsResponse = await until(async () => {
      const res = await api.services.client.listEvents({
        instance_page_id: draft1.target_page_id,
        include_system: true,
      });
      const comments = extractReviewComments(res.events, draft1.draft_id);
      return comments.some((c) => c.body === commentBody1) ? res : undefined;
    });

    // Review model filters comments strictly for the active draft.
    const commentsDraft1 = extractReviewComments(eventsResponse.events, draft1.draft_id);
    const comment = commentsDraft1.find((c) => c.body === commentBody1);
    assert.ok(comment, 'comment for draft 1 must be present');
    assert.equal(comment.line, commentLine1);
    assert.ok(comment.author, 'author must be stamped by the gateway');
    assert.notEqual(comment.author, '');

    // Isolation: comments for other drafts must not leak into draft 1 review threads.
    const eventsResponse2 = await api.services.client.listEvents({
      instance_page_id: draft2.target_page_id,
      include_system: true,
    });
    const combinedEvents = [...eventsResponse.events, ...eventsResponse2.events];
    const isolatedComments = extractReviewComments(combinedEvents, draft1.draft_id);
    assert.ok(
      !isolatedComments.some((c) => c.body === commentBody2),
      'comment naming another draft must not be returned for draft 1',
    );

    // Thread grouping anchors comments by line number.
    const threads = buildReviewCommentThreads(eventsResponse.events, draft1.draft_id);
    const thread = threads.find((t) => t.line === commentLine1);
    assert.ok(thread, 'thread must be formed at the anchored line');
    assert.ok(thread.comments.some((c) => c.body === commentBody1));
  });

  test('Promote the changeset in one action promotes all drafts and clears Awaiting you', async () => {
    const { changesetId: csId, targets: targetList } = await ensureThreeDraftChangeset();

    api.awaiting.refresh();
    const rows = await until(async () => {
      const items = await api.awaiting.getChildren();
      return items.some((i) => i.kind === 'changeset' && i.id === csId) ? items : undefined;
    });

    const csRow = rows.find((r): r is ChangesetRow => r.kind === 'changeset' && r.id === csId);
    assert.ok(csRow, 'changeset row must exist in awaiting view');

    // Executing promote on the changeset row promotes all held drafts in one atomic action.
    await vscode.commands.executeCommand('escurel.promote', csRow);

    // Target instances must now reflect the new content.
    for (const target of targetList) {
      const page = await until(async () => {
        const exp = await api.services.client.expand({ page_id: target.pageId, raw: true });
        return exp.content === target.draftContent ? exp : undefined;
      });
      assert.equal(page.content, target.draftContent);
    }

    // Awaiting queue must be cleared of the promoted changeset. Wrapped in an
    // object because the queue legitimately ends up empty here, and `until`
    // reads a bare empty array as "not settled yet".
    const { items: remaining } = await until(async () => {
      const items = await api.awaiting.getChildren();
      return items.every((i) => !('id' in i) || i.id !== csId) ? { items } : undefined;
    });
    assert.ok(
      !remaining.some((i) => 'id' in i && i.id === csId),
      'promoted changeset must not appear in Awaiting you',
    );
  });

  test('Promote the same changeset again is treated as already decided rather than an error', async () => {
    const { changesetId: csId } = await ensureThreeDraftChangeset();

    // Ensure the changeset has been promoted before verifying already_decided handling.
    const initialCheck = await api.services.client.listChangesets();
    const cs = initialCheck.find((c) => c.changeset_id === csId);
    if (cs && cs.status === 'open') {
      await api.services.client.promoteChangeset({ changeset_id: csId });
    }

    // Repeated promotion must return already_decided without raising an error.
    const res = await api.services.client.promoteChangeset({ changeset_id: csId });
    const outcome = interpretPromoteChangesetResult(res);

    assert.equal(
      outcome.kind,
      'already_decided',
      'review model must classify outcome as already_decided',
    );
    assert.ok(outcome.message.includes(csId));
    assert.equal(outcome.closeDiff, true);
    assert.equal(outcome.refresh, true);

    // The VS Code command must handle already_decided as an informational notice.
    await vscode.commands.executeCommand('escurel.promote', { changesetId: csId });
  });
});
