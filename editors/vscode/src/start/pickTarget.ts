import * as vscode from 'vscode';
import type {
  EscurelClient,
  Instance,
  ListInstancesRequest,
  ListInstancesResponse,
} from '../client';
import { pageSlug } from '../shared/pageId';

const TITLE_KEYS = ['title', 'name', 'summary', 'subject', 'label'];

export interface TargetItem {
  label: string;
  description?: string;
  detail?: string;
  pageId?: string;
  isNoTarget?: boolean;
  isLoadMore?: boolean;
}

export const NO_TARGET_ITEM: TargetItem = {
  label: '$(dash) No target instance',
  description: 'Run without a target page',
  isNoTarget: true,
  pageId: '',
};

export function formatTargetItem(inst: Instance, skill: string): TargetItem {
  const slug = pageSlug(inst.page_id, skill);
  const fm = inst.frontmatter ?? {};
  const title = TITLE_KEYS.map((k) => fm[k]).find(
    (v) => typeof v === 'string' && v.trim() && v !== slug,
  ) as string | undefined;

  return {
    label: slug,
    description: title ?? '',
    detail: inst.page_id,
    pageId: inst.page_id,
  };
}

export type TargetSelectionResult =
  | { action: 'select'; pageId: string }
  | { action: 'no-target'; pageId: '' }
  | { action: 'load-more' };

export function resolveTargetSelection(item: TargetItem): TargetSelectionResult {
  if (item.isLoadMore) {
    return { action: 'load-more' };
  }
  if (item.isNoTarget) {
    return { action: 'no-target', pageId: '' };
  }
  return { action: 'select', pageId: item.pageId ?? '' };
}

export interface TargetModelOptions {
  skill: string;
  fetchPage: (req: ListInstancesRequest) => Promise<ListInstancesResponse>;
  pageSize?: number;
}

export class TargetModel {
  private instances: Instance[] = [];
  private nextCursor: string | null = null;
  private loaded = false;
  private query = '';

  constructor(private readonly options: TargetModelOptions) {}

  get hasMore(): boolean {
    return !this.loaded || this.nextCursor !== null;
  }

  async loadNextPage(): Promise<TargetItem[]> {
    const res = await this.options.fetchPage({
      skill_id: this.options.skill,
      cursor: this.nextCursor ?? undefined,
      limit: this.options.pageSize,
    });
    this.instances.push(...res.instances);
    this.nextCursor = res.next_cursor;
    this.loaded = true;
    return this.visibleItems();
  }

  setQuery(q: string): TargetItem[] {
    this.query = q.trim().toLowerCase();
    return this.visibleItems();
  }

  visibleItems(): TargetItem[] {
    const items: TargetItem[] = [];

    // "No target" option: show when empty or matching "no target instance"
    if (!this.query || 'no target instance'.includes(this.query)) {
      items.push({ ...NO_TARGET_ITEM });
    }

    // Filter instances by query
    for (const inst of this.instances) {
      const item = formatTargetItem(inst, this.options.skill);
      if (
        !this.query ||
        item.label.toLowerCase().includes(this.query) ||
        (item.description && item.description.toLowerCase().includes(this.query)) ||
        (item.detail && item.detail.toLowerCase().includes(this.query))
      ) {
        items.push(item);
      }
    }

    // "Load more" option when more instances are available on the gateway
    if (this.nextCursor !== null) {
      items.push({
        label: '$(ellipsis) Load more instances…',
        description: 'Fetch next page of instances',
        isLoadMore: true,
      });
    }

    return items;
  }
}

/**
 * QuickPick for selecting a target instance for `skill`.
 * Returns the selected `page_id`, `""` for no target, or `undefined` if cancelled.
 */
export async function pickTarget(
  client: EscurelClient,
  skill: string,
): Promise<string | undefined> {
  const model = new TargetModel({
    skill,
    fetchPage: (req) => client.listInstancesPage(req),
  });

  const qp = vscode.window.createQuickPick<TargetItem & vscode.QuickPickItem>();
  qp.placeholder = `Select target instance for ${skill} (or search…)`;
  qp.matchOnDescription = true;
  qp.matchOnDetail = true;
  qp.busy = true;
  qp.show();

  const updateItems = () => {
    qp.items = model.visibleItems() as (TargetItem & vscode.QuickPickItem)[];
  };

  try {
    await model.loadNextPage();
    qp.busy = false;
    updateItems();

    return await new Promise<string | undefined>((resolve) => {
      qp.onDidChangeValue((val) => {
        model.setQuery(val);
        updateItems();
      });

      qp.onDidAccept(async () => {
        const selected = qp.selectedItems[0];
        if (!selected) {
          resolve(undefined);
          qp.dispose();
          return;
        }

        const decision = resolveTargetSelection(selected);
        if (decision.action === 'load-more') {
          qp.busy = true;
          await model.loadNextPage();
          qp.busy = false;
          updateItems();
          return;
        }

        resolve(decision.pageId);
        qp.dispose();
      });

      qp.onDidHide(() => {
        resolve(undefined);
        qp.dispose();
      });
    });
  } catch (err) {
    qp.dispose();
    throw err;
  }
}
