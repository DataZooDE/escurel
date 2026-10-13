import { describe, expect, it } from 'vitest';
import type { Event } from '../../src/client';
import {
  allowedHarnesses,
  estimateHeartbeatIntervalMs,
  deriveHealth,
  type RunnerStatusBody,
} from '../../src/views/runnerModel';

import runnerStatusFixture from './fixtures/runner/runner-status-newest3.json';

describe('runnerModel', () => {
  const newestStatusEvent = (runnerStatusFixture as { events: Event[] }).events[0]!;
  // newestStatusEvent.at is "2026-10-02T11:57:42Z"
  const eventTime = new Date('2026-10-02T11:57:42Z').getTime();

  describe('deriveHealth', () => {
    it('returns "none" / "no runner status yet" when statusRow is null or undefined', () => {
      const h1 = deriveHealth(null);
      expect(h1.status).toBe('none');
      expect(h1.label).toBe('no runner status yet');

      const h2 = deriveHealth(undefined);
      expect(h2.status).toBe('none');
      expect(h2.label).toBe('no runner status yet');
    });

    it('returns "ok" and heartbeat age description when heartbeat is fresh', () => {
      // 4 seconds after heartbeat
      const now4s = new Date(eventTime + 4000);
      const health = deriveHealth(newestStatusEvent, now4s);
      expect(health.status).toBe('ok');
      expect(health.label).toBe('ok');
      expect(health.description).toBe('last heartbeat 4 s ago');
    });

    it('returns "stale" when age exceeds 3x heartbeat interval (90s)', () => {
      // 95 seconds after heartbeat
      const now95s = new Date(eventTime + 95000);
      const health = deriveHealth(newestStatusEvent, now95s);
      expect(health.status).toBe('stale');
      expect(health.label).toBe('stale');
      expect(health.description).toBe('last heartbeat 95 s ago');
    });

    it('returns "stale" when last_poll_age_ms is huge even if heartbeat timestamp looks fresh', () => {
      const now4s = new Date(eventTime + 4000);
      const staleBody: RunnerStatusBody = {
        ...JSON.parse(newestStatusEvent.body!),
        last_poll_age_ms: 120000, // 120 seconds
      };
      const health = deriveHealth(
        { at: newestStatusEvent.at, body: JSON.stringify(staleBody) },
        now4s,
      );
      expect(health.status).toBe('stale');
      expect(health.label).toBe('stale');
    });

    it('returns "draining" when draining is true', () => {
      const drainingBody: RunnerStatusBody = {
        ...JSON.parse(newestStatusEvent.body!),
        draining: true,
      };
      const now4s = new Date(eventTime + 4000);
      const health = deriveHealth(
        { at: newestStatusEvent.at, body: JSON.stringify(drainingBody) },
        now4s,
      );
      expect(health.status).toBe('draining');
      expect(health.label).toBe('draining');
    });
  });

  describe('allowedHarnesses', () => {
    it('returns undefined when the runner published no list (old runner)', () => {
      expect(allowedHarnesses(null)).toBeUndefined();
      expect(allowedHarnesses(undefined)).toBeUndefined();
      expect(allowedHarnesses({ harness: 'echo' })).toBeUndefined();
      expect(allowedHarnesses({ harness_allow: 'echo' as unknown as string[] })).toBeUndefined();
    });

    it('passes the published names through, deduped', () => {
      expect(
        allowedHarnesses({ harness: 'echo', harness_allow: ['claude', 'echo', 'claude'] }),
      ).toEqual(['claude', 'echo']);
    });

    it('treats status text as untrusted: cleans, drops the over-long, caps at 20', () => {
      const allow = [
        '  claude  ',
        'bad\u202Aname',
        'x'.repeat(41),
        7,
        '',
        ...Array.from({ length: 25 }, (_, i) => `h${i}`),
      ] as string[];
      const names = allowedHarnesses({ harness_allow: allow })!;
      expect(names.length).toBeLessThanOrEqual(20);
      expect(names).toContain('claude');
      expect(names).toContain('badname');
      for (const name of names) {
        expect(name.length).toBeLessThanOrEqual(40);
        // eslint-disable-next-line no-control-regex
        expect(name).not.toMatch(/[\u0000-\u001f\u202a-\u202e]/);
      }
    });
  });
});

describe('the heartbeat interval is observed, not assumed', () => {
  // `ESCUREL_RUNNER_STATUS_INTERVAL` is configurable and the status body does not carry it, so a
  // fixed 30 s would call a healthy runner with a 60 s heartbeat "stale". The rows themselves show
  // it: the gaps between consecutive `heartbeat` rows.
  const template = (runnerStatusFixture.events as Event[])[2]!; // a recorded heartbeat row
  const heartbeatAt = (iso: string, title = 'heartbeat') => ({ ...template, at: iso, title });

  it('is the median gap between heartbeat rows, newest first or not', () => {
    const rows = [
      heartbeatAt('2026-10-02T12:03:00Z'),
      heartbeatAt('2026-10-02T12:02:00Z'),
      heartbeatAt('2026-10-02T12:01:00Z'),
      heartbeatAt('2026-10-02T12:00:00Z'),
    ];
    expect(estimateHeartbeatIntervalMs(rows)).toBe(60_000);
  });

  it('ignores `changed` rows: they arrive whenever something changes, not on the heartbeat', () => {
    const rows = [
      heartbeatAt('2026-10-02T12:01:00Z'),
      heartbeatAt('2026-10-02T12:00:50Z', 'changed'),
      heartbeatAt('2026-10-02T12:00:00Z'),
    ];
    expect(estimateHeartbeatIntervalMs(rows)).toBe(60_000);
  });

  it('falls back to the runner default (30 s) without two heartbeats to measure', () => {
    expect(estimateHeartbeatIntervalMs([])).toBe(30_000);
    expect(estimateHeartbeatIntervalMs([heartbeatAt('2026-10-02T12:00:00Z')])).toBe(30_000);
  });

  it('judges staleness by it: 100 s is fine for a 60 s heartbeat and stale for a 30 s one', () => {
    const row = heartbeatAt('2026-10-02T12:00:00Z');
    const now = Date.parse('2026-10-02T12:01:40Z');
    expect(deriveHealth(row, now, { intervalMs: 60_000 }).status).toBe('ok');
    expect(deriveHealth(row, now, { intervalMs: 30_000 }).status).toBe('stale');
  });
});

describe('health ages with the clock', () => {
  // A runner that dies after one good heartbeat sends nothing more. What the view shows must
  // therefore be a function of NOW, so redrawing it on a timer is what turns "ok" into "stale".
  it('is ok just after a heartbeat and stale once enough time has passed with none', () => {
    const row = (runnerStatusFixture.events as Event[])[2]!;
    const at = Date.parse(row.at as string);
    expect(deriveHealth(row, at + 5_000).status).toBe('ok');
    expect(deriveHealth(row, at + 10 * 60_000).status).toBe('stale');
  });
});
