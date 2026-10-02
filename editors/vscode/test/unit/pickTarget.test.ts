import { describe, expect, it } from 'vitest';
import type { Instance } from '../../src/client/types';
import {
  NO_TARGET_ITEM,
  TargetModel,
  formatTargetItem,
  resolveTargetSelection,
} from '../../src/start/pickTarget';

function makeInstance(id: string, skill: string, title?: string): Instance {
  return {
    page_id: `markdown/instances/${skill}__${id}.md`,
    skill,
    at: null,
    frontmatter: title ? { title } : {},
  };
}

describe('formatTargetItem', () => {
  it('formats an instance with slug, title description, and pageId detail', () => {
    const inst = makeInstance('order-123', 'customer-order', 'Widget Delivery');
    const item = formatTargetItem(inst, 'customer-order');
    expect(item.label).toBe('order-123');
    expect(item.description).toBe('Widget Delivery');
    expect(item.detail).toBe('markdown/instances/customer-order__order-123.md');
    expect(item.pageId).toBe('markdown/instances/customer-order__order-123.md');
  });

  it('handles nested instance page ids', () => {
    const inst: Instance = {
      page_id: 'markdown/instances/renewal/c1.md',
      skill: 'renewal',
      at: null,
      frontmatter: {},
    };
    const item = formatTargetItem(inst, 'renewal');
    expect(item.label).toBe('c1');
    expect(item.pageId).toBe('markdown/instances/renewal/c1.md');
  });
});

describe('TargetModel', () => {
  it('loads first page and exposes "no target" plus instance items', async () => {
    const instancesPage1 = [
      makeInstance('o1', 'customer-order', 'Order 1'),
      makeInstance('o2', 'customer-order', 'Order 2'),
    ];

    const model = new TargetModel({
      skill: 'customer-order',
      fetchPage: async ({ cursor }) => {
        expect(cursor).toBeUndefined();
        return { instances: instancesPage1, next_cursor: 'cur-1' };
      },
    });

    expect(model.hasMore).toBe(true);
    await model.loadNextPage();

    const items = model.visibleItems();
    // Should include: "no target", 2 instances, and "load more"
    expect(items[0]!.isNoTarget).toBe(true);
    expect(items[0]!.label).toContain('No target instance');
    expect(items[1]!.pageId).toBe('markdown/instances/customer-order__o1.md');
    expect(items[2]!.pageId).toBe('markdown/instances/customer-order__o2.md');
    expect(items[3]!.isLoadMore).toBe(true);
  });

  it('pages instances until next_cursor is null', async () => {
    const pages: Record<string, { instances: Instance[]; next_cursor: string | null }> = {
      initial: {
        instances: [makeInstance('o1', 'customer-order')],
        next_cursor: 'cur-2',
      },
      'cur-2': {
        instances: [makeInstance('o2', 'customer-order')],
        next_cursor: null,
      },
    };

    const model = new TargetModel({
      skill: 'customer-order',
      fetchPage: async ({ cursor }) => {
        return pages[cursor ?? 'initial'] ?? { instances: [], next_cursor: null };
      },
    });

    await model.loadNextPage();
    expect(model.hasMore).toBe(true);
    expect(model.visibleItems().some((i) => i.isLoadMore)).toBe(true);

    await model.loadNextPage();
    expect(model.hasMore).toBe(false);
    expect(model.visibleItems().some((i) => i.isLoadMore)).toBe(false);
    expect(model.visibleItems().filter((i) => i.pageId).length).toBe(2);
  });

  it('filters items by search query across label, description, and detail', async () => {
    const instances = [
      makeInstance('alpha-100', 'customer-order', 'High priority shipment'),
      makeInstance('beta-200', 'customer-order', 'Standard delivery'),
      makeInstance('gamma-300', 'customer-order', 'Express cargo'),
    ];

    const model = new TargetModel({
      skill: 'customer-order',
      fetchPage: async () => ({ instances, next_cursor: null }),
    });
    await model.loadNextPage();

    // Matching label
    model.setQuery('alpha');
    let visible = model.visibleItems();
    expect(visible.map((i) => i.label)).toEqual(['alpha-100']);

    // Matching description
    model.setQuery('priority');
    visible = model.visibleItems();
    expect(visible.map((i) => i.label)).toEqual(['alpha-100']);

    // Matching detail (page id)
    model.setQuery('gamma-300');
    visible = model.visibleItems();
    expect(visible.map((i) => i.label)).toEqual(['gamma-300']);

    // Resetting query returns all plus no-target
    model.setQuery('');
    visible = model.visibleItems();
    expect(visible.length).toBe(4); // no-target + 3 instances
  });

  it('allows matching the "no target" option when searching for target/no', async () => {
    const instances = [makeInstance('o1', 'customer-order')];
    const model = new TargetModel({
      skill: 'customer-order',
      fetchPage: async () => ({ instances, next_cursor: null }),
    });
    await model.loadNextPage();

    model.setQuery('no target');
    const visible = model.visibleItems();
    expect(visible.length).toBe(1);
    expect(visible[0]!.isNoTarget).toBe(true);
  });
});

describe('resolveTargetSelection', () => {
  it('resolves NO_TARGET_ITEM to no-target action with empty pageId', () => {
    const res = resolveTargetSelection(NO_TARGET_ITEM);
    expect(res).toEqual({ action: 'no-target', pageId: '' });
  });

  it('resolves instance item to select action with pageId', () => {
    const item = formatTargetItem(makeInstance('o1', 'customer-order'), 'customer-order');
    const res = resolveTargetSelection(item);
    expect(res).toEqual({
      action: 'select',
      pageId: 'markdown/instances/customer-order__o1.md',
    });
  });

  it('resolves load more item to load-more action', () => {
    const res = resolveTargetSelection({
      label: 'Load more',
      isLoadMore: true,
    });
    expect(res).toEqual({ action: 'load-more' });
  });
});
