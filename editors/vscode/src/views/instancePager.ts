export interface PageResult<R> {
  rows: R[];
  /** `null` = no more pages. */
  next: string | null;
}

export interface Page<R> {
  rows: R[];
  cursor: string | null;
}

/**
 * The instances of each skill, loaded a page at a time, safe against the ways a tree provider is called:
 * the same skill asked for twice while a fetch is in flight, a double click on "Load more…", and a
 * refresh that clears the cache while an older fetch is still running.
 *
 * - concurrent callers share one in-flight fetch (the rows are appended once),
 * - a "load more" is only honoured for the CURRENT cursor (a repeat is a no-op),
 * - `reset()` retires everything in flight: a stale result never repopulates the cache,
 * - rows are de-duplicated by key (VS Code treats a repeated tree id as an error),
 * - a failed fetch is forgotten, so the next ask retries; a failed "load more" rejects and keeps the page.
 */
export class InstancePager<R> {
  private readonly pages = new Map<string, Page<R>>();
  private readonly inFlight = new Map<string, Promise<Page<R>>>();
  private generation = 0;

  constructor(
    private readonly fetchPage: (
      skill: string,
      cursor: string | undefined,
    ) => Promise<PageResult<R>>,
    private readonly keyOf: (row: R) => string,
  ) {}

  get(skill: string): Page<R> | undefined {
    return this.pages.get(skill);
  }

  /** The first page of a skill: cached, in flight, or fetched now. */
  first(skill: string): Promise<Page<R>> {
    const cached = this.pages.get(skill);
    if (cached) return Promise.resolve(cached);
    return this.run(`${skill}\u0000`, skill, undefined);
  }

  /** The page after `cursor`, only if `cursor` is still where the skill's rows end. */
  async more(skill: string, cursor: string): Promise<Page<R> | undefined> {
    const current = this.pages.get(skill);
    if (!current || current.cursor !== cursor) return current;
    return this.run(`${skill}\u0000${cursor}`, skill, cursor);
  }

  /** Forget everything, and retire every fetch in flight. */
  reset(): void {
    this.generation += 1;
    this.pages.clear();
    this.inFlight.clear();
  }

  private run(key: string, skill: string, cursor: string | undefined): Promise<Page<R>> {
    const existing = this.inFlight.get(key);
    if (existing) return existing;
    const mine = this.generation;
    const started = (async () => {
      try {
        const res = await this.fetchPage(skill, cursor);
        if (mine !== this.generation) return { rows: res.rows, cursor: res.next };
        const prev = this.pages.get(skill)?.rows ?? [];
        const seen = new Set(prev.map(this.keyOf));
        const fresh = res.rows.filter((r) => !seen.has(this.keyOf(r)) && seen.add(this.keyOf(r)));
        const page = { rows: [...prev, ...fresh], cursor: res.next };
        this.pages.set(skill, page);
        return page;
      } finally {
        if (mine === this.generation) this.inFlight.delete(key);
      }
    })();
    this.inFlight.set(key, started);
    return started;
  }
}
