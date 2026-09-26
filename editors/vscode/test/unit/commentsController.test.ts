import { describe, expect, it, vi } from 'vitest';
import type { Draft, EscurelClient, Event } from '../../src/client';
import { ReviewCommentsController } from '../../src/review/commentsController';
import type { CommentController } from './mocks/vscode';

describe('ReviewCommentsController', () => {
  const draft: Draft = {
    draft_id: '01M3CAHP17503GSZN7QRRA03GA',
    target_page_id: 'markdown/instances/engagement__ha-spine.md',
    content: '# Hoffmann Automotive S&OP arc\n\nLine 3\nLine 4\n',
    content_sha256: '5d4aaebe44d75d6f68c6c5468e41304ba7d5e915eb9702792cf3922ae93fea7f',
    base_sha256: null,
    author: 'anonymous',
    event_id: '01M3C286Y4T8QTQPJ4DDWA6S09',
    changeset_id: '01M3CAHP14HT8AGG0CH60H73HM',
    status: 'open',
    reason: null,
    decided_by: null,
    created_at: '2026-09-25T13:02:19Z',
    base_version: 'v3',
    run_id: null,
    root_event_id: null,
  };

  const initialEvents: Event[] = [
    {
      event_id: 'ev-c0',
      at: '2026-09-26T02:50:00Z',
      source: 'workbench',
      mime: 'text/plain',
      label_skill: 'escurel:review-comment',
      instance_page_id: 'markdown/instances/engagement__ha-spine.md',
      status: 'inbox',
      title: '',
      body: 'top-level summary comment on draft overall',
      provenance: {
        review: { draft_id: draft.draft_id, commented_by: 'reviewer' },
      },
      kind: 'user',
      root_event_id: 'ev-c0',
      run_id: null,
    },
    {
      event_id: 'ev-c1',
      at: '2026-09-26T03:00:27Z',
      source: 'workbench',
      mime: 'text/plain',
      label_skill: 'escurel:review-comment',
      instance_page_id: 'markdown/instances/engagement__ha-spine.md',
      status: 'inbox',
      title: '',
      body: 'the second paragraph overstates the risk',
      provenance: {
        review: { draft_id: draft.draft_id, line: 4, commented_by: 'reviewer' },
      },
      kind: 'user',
      root_event_id: 'ev-c1',
      run_id: null,
    },
  ];

  it('creates exactly one set of threads when loaded twice concurrently', async () => {
    const listEventsMock = vi.fn().mockImplementation(async () => {
      // Small delay ensures both overlapping calls enter flight before either completes.
      await new Promise((resolve) => setTimeout(resolve, 10));
      return { events: initialEvents };
    });

    const mockClient = {
      listEvents: listEventsMock,
    } as unknown as EscurelClient;

    const controller = new ReviewCommentsController(() => mockClient);
    const mockVscodeController = controller.commentController as unknown as CommentController;

    // Simulate overlapping calls (e.g. openDraftReview diff open + onDidOpenTextDocument)
    await Promise.all([
      controller.loadCommentsForDraft(draft),
      controller.loadCommentsForDraft(draft),
    ]);

    // Initial events contain 2 comments on different lines (unanchored and line 4),
    // which map to exactly 2 comment threads in the editor.
    expect(mockVscodeController.threads.length).toBe(2);
    expect(mockVscodeController.threads.every((t) => !t.disposed)).toBe(true);

    controller.dispose();
  });

  it('replaces threads with the new state when reloaded after comments genuinely change', async () => {
    let currentEvents = [...initialEvents];
    const mockClient = {
      listEvents: vi.fn().mockImplementation(async () => ({ events: currentEvents })),
    } as unknown as EscurelClient;

    const controller = new ReviewCommentsController(() => mockClient);
    const mockVscodeController = controller.commentController as unknown as CommentController;

    await controller.loadCommentsForDraft(draft);
    expect(mockVscodeController.threads.length).toBe(2);
    const firstThreads = [...mockVscodeController.threads];

    // A third comment arrives on a new line (line 3).
    const newCommentEvent: Event = {
      event_id: 'ev-c2',
      at: '2026-09-26T03:10:00Z',
      source: 'workbench',
      mime: 'text/plain',
      label_skill: 'escurel:review-comment',
      instance_page_id: 'markdown/instances/engagement__ha-spine.md',
      status: 'inbox',
      title: '',
      body: 'check typo on line 3',
      provenance: {
        review: { draft_id: draft.draft_id, line: 3, commented_by: 'reviewer' },
      },
      kind: 'user',
      root_event_id: 'ev-c2',
      run_id: null,
    };
    currentEvents = [...initialEvents, newCommentEvent];

    await controller.loadCommentsForDraft(draft);

    // Old threads must be disposed and replaced with the new set of 3 threads.
    expect(firstThreads.every((t) => t.disposed)).toBe(true);
    expect(mockVscodeController.threads.length).toBe(3);
    expect(mockVscodeController.threads.every((t) => !t.disposed)).toBe(true);

    controller.dispose();
  });
});
