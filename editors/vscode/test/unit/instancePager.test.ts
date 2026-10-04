import { describe, expect, it } from 'vitest';
import { InstancePager, type PageResult } from '../../src/views/instancePager';

type Row = { id: string };
const row = (id: string): Row => ({ id });
const deferred = <T>() => {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
};

// The tree used to append to whatever was cached AFTER an await: expanding a skill while a refresh was in
// flight, or double-clicking "Load more…", doubled the rows (two rows with one id: VS Code reports a
// duplicate id and renders a broken subtree), and a result for a cleared cache repopulated it.
describe('InstancePager', () => {
  const make = (
    fetchPage: (skill: string, cursor: string | undefined) => Promise<PageResult<Row>>,
  ) => new InstancePager<Row>(fetchPage, (r) => r.id);

  it('fetches a skill once however many callers ask while it is in flight', async () => {
    const d = deferred<PageResult<Row>>();
    let calls = 0;
    const pager = make(() => {
      calls += 1;
      return d.promise;
    });
    const a = pager.first('order');
    const b = pager.first('order');
    d.resolve({ rows: [row('1'), row('2')], next: null });
    expect((await a).rows.map((r) => r.id)).toEqual(['1', '2']);
    expect((await b).rows.map((r) => r.id)).toEqual(['1', '2']);
    expect(calls).toBe(1);
  });

  it('a double click on Load more adds the page once', async () => {
    const d = deferred<PageResult<Row>>();
    const cursors: (string | undefined)[] = [];
    const pager = make((_s, cursor) => {
      cursors.push(cursor);
      return cursor === undefined ? Promise.resolve({ rows: [row('1')], next: 'c1' }) : d.promise;
    });
    await pager.first('order');
    const a = pager.more('order', 'c1');
    const b = pager.more('order', 'c1');
    d.resolve({ rows: [row('2')], next: null });
    await Promise.all([a, b]);
    expect(pager.get('order')?.rows.map((r) => r.id)).toEqual(['1', '2']);
    expect(cursors).toEqual([undefined, 'c1']);
  });

  it('ignores a Load more for a cursor that is no longer the current one', async () => {
    const pager = make((_s, cursor) =>
      Promise.resolve(
        cursor === undefined ? { rows: [row('1')], next: 'c1' } : { rows: [row('2')], next: null },
      ),
    );
    await pager.first('order');
    await pager.more('order', 'c1');
    await pager.more('order', 'c1');
    expect(pager.get('order')?.rows.map((r) => r.id)).toEqual(['1', '2']);
  });

  it('a refresh retires a fetch in flight: its result never lands in the new cache', async () => {
    const old = deferred<PageResult<Row>>();
    let n = 0;
    const pager = make(() =>
      n++ === 0 ? old.promise : Promise.resolve({ rows: [row('new')], next: null }),
    );
    const stale = pager.first('order');
    pager.reset();
    old.resolve({ rows: [row('old')], next: null });
    await stale;
    expect(pager.get('order')).toBeUndefined();
    expect((await pager.first('order')).rows.map((r) => r.id)).toEqual(['new']);
  });

  it('keeps one row per key even if the source repeats one across pages', async () => {
    const pager = make((_s, cursor) =>
      Promise.resolve(
        cursor === undefined
          ? { rows: [row('1'), row('2')], next: 'c1' }
          : { rows: [row('2'), row('3')], next: null },
      ),
    );
    await pager.first('order');
    await pager.more('order', 'c1');
    expect(pager.get('order')?.rows.map((r) => r.id)).toEqual(['1', '2', '3']);
  });

  it('a failed fetch is not remembered: the next ask tries again, and a failed Load more rejects', async () => {
    let n = 0;
    const pager = make(() =>
      n++ === 0
        ? Promise.reject(new Error('429'))
        : Promise.resolve({ rows: [row('1')], next: 'c1' }),
    );
    await expect(pager.first('order')).rejects.toThrow('429');
    expect((await pager.first('order')).rows).toHaveLength(1);
    const failing = make((_s, cursor) =>
      cursor ? Promise.reject(new Error('401')) : Promise.resolve({ rows: [row('1')], next: 'c1' }),
    );
    await failing.first('order');
    await expect(failing.more('order', 'c1')).rejects.toThrow('401');
    expect(failing.get('order')?.cursor).toBe('c1');
  });
});
