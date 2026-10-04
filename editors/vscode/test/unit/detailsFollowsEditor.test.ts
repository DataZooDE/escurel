import { describe, expect, it } from 'vitest';
import { isThreadViewType, shouldShowDetails } from '../../src/thread/detailsFollowsEditor';

// The Details panel showed a run while an order page was the open editor. It follows the editor:
// it shows a node only while a THREAD tab is the one in front.
describe('isThreadViewType', () => {
  it('recognises the thread webview panel however VS Code prefixes the view type', () => {
    expect(isThreadViewType('escurel.thread')).toBe(true);
    expect(isThreadViewType('mainThreadWebview-escurel.thread')).toBe(true);
    expect(isThreadViewType('mainThreadWebview-escurel.run')).toBe(false);
    expect(isThreadViewType('escurel.pageAsUi')).toBe(false);
    expect(isThreadViewType(undefined)).toBe(false);
  });
});

describe('shouldShowDetails', () => {
  it('shows the selected node when a thread is in front', () => {
    expect(
      shouldShowDetails({ hasSelection: true, activeIsThread: true, activeIsNone: false }),
    ).toBe(true);
  });
  it('clears for any other editor (a page, a run, a text file)', () => {
    expect(
      shouldShowDetails({ hasSelection: true, activeIsThread: false, activeIsNone: false }),
    ).toBe(false);
  });
  it('keeps what it shows when there is no editor at all (focus is in the panel itself)', () => {
    expect(
      shouldShowDetails({ hasSelection: true, activeIsThread: false, activeIsNone: true }),
    ).toBe(true);
  });
  it('shows nothing without a selection', () => {
    expect(
      shouldShowDetails({ hasSelection: false, activeIsThread: true, activeIsNone: false }),
    ).toBe(false);
  });
});
