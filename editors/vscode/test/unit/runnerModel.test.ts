import { describe, expect, it } from 'vitest';
import type { Event } from '../../src/client';
import {
  buildRunnerRows,
  deriveHealth,
  extractDeadLetters,
  parseRunnerStatusBody,
  type RunnerStatusBody,
} from '../../src/views/runnerModel';

import runnerStatusFixture from './fixtures/runner/runner-status-newest3.json';
import runRowsFixture from './fixtures/runner/run-rows-newest30.json';
import runStartedDeadletterFixture from './fixtures/runner/run-started-deadletter.json';
import adminQuotaAdminFixture from './fixtures/runner/admin-quota-admin.json';
import adminQuotaHumanFixture from './fixtures/runner/admin-quota-human.json';

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

  describe('buildRunnerRows with recorded status fixture', () => {
    const statusBody = parseRunnerStatusBody(newestStatusEvent)!;
    const now4s = new Date(eventTime + 4000);

    it('builds health, runner, runs, and permits rows from the real status body', () => {
      const rows = buildRunnerRows(newestStatusEvent, [], { admin: 'not-admin' }, now4s);

      // Health row
      const healthRow = rows.find((r) => r.kind === 'health');
      expect(healthRow).toBeDefined();
      expect(healthRow?.label).toBe('Health');
      expect(healthRow?.description).toContain('ok');
      expect(healthRow?.description).toContain('last heartbeat 4 s ago');

      // Runner row: runner_id, version, harness, tenant, uptime_s
      const runnerRow = rows.find((r) => r.kind === 'runner');
      expect(runnerRow).toBeDefined();
      expect(runnerRow?.description).toContain('runner:1150355');
      expect(runnerRow?.description).toContain('1.0.0');
      expect(runnerRow?.description).toContain('echo');
      expect(runnerRow?.description).toContain('vsx');
      expect(runnerRow?.description).toContain('740');

      // Runs row: live_runs.length live, then processed, failed, dead_letter, cancelled, planned, pending
      const runsRow = rows.find((r) => r.kind === 'runs');
      expect(runsRow).toBeDefined();
      expect(runsRow?.description).toContain('0 live');
      expect(runsRow?.description).toContain('3 processed');
      expect(runsRow?.description).toContain('0 failed');
      expect(runsRow?.description).toContain('1 dead letter');

      // Permits row
      const permitsRow = rows.find((r) => r.kind === 'permits');
      expect(permitsRow).toBeDefined();
      expect(permitsRow?.description).toContain('16 available');

      // Throttled row: in real recording all throttled counts are 0, so omitted
      const throttledRow = rows.find((r) => r.kind === 'throttled');
      expect(throttledRow).toBeUndefined();

      // Paused row: in real recording paused_tenants is empty, so omitted
      const pausedRow = rows.find((r) => r.kind === 'paused');
      expect(pausedRow).toBeUndefined();
    });

    it('includes throttled row ONLY when any throttled count > 0', () => {
      const throttledStatus: RunnerStatusBody = {
        ...statusBody,
        throttled: {
          max_concurrent: 0,
          paused: 0,
          runs_per_min: 5,
        },
      };

      const rows = buildRunnerRows(throttledStatus, [], { admin: 'not-admin' }, now4s);

      const throttledRow = rows.find((r) => r.kind === 'throttled');
      expect(throttledRow).toBeDefined();
      expect(throttledRow?.description).toContain('5');
    });

    it('builds paused tenants rows with contextValue "paused"', () => {
      const pausedStatus: RunnerStatusBody = {
        ...statusBody,
        paused_tenants: ['alpha-corp', 'beta-inc'],
      };

      const rows = buildRunnerRows(pausedStatus, [], { admin: 'not-admin' }, now4s);

      const pausedRow = rows.find((r) => r.kind === 'paused');
      expect(pausedRow).toBeDefined();
      expect(pausedRow?.children).toHaveLength(2);
      expect(pausedRow?.children?.[0]?.label).toBe('alpha-corp');
      expect(pausedRow?.children?.[0]?.contextValue).toBe('paused');
      expect(pausedRow?.children?.[1]?.label).toBe('beta-inc');
      expect(pausedRow?.children?.[1]?.contextValue).toBe('paused');
    });

    it('builds live runs rows carrying runId and contextValue "liveRun"', () => {
      const liveStatus: RunnerStatusBody = {
        ...statusBody,
        live_runs: [
          {
            run_id: 'run-999',
            event_id: 'evt-999',
            instance_page_id: 'markdown/instances/order__order-101.md',
          },
        ],
      };

      const rows = buildRunnerRows(liveStatus, [], { admin: 'not-admin' }, now4s);

      const liveGroup = rows.find((r) => r.kind === 'liveRuns');
      expect(liveGroup).toBeDefined();
      expect(liveGroup?.children).toHaveLength(1);
      const child = liveGroup?.children?.[0];
      expect(child?.label).toBe('order-101');
      expect(child?.runId).toBe('run-999');
      expect(child?.contextValue).toBe('liveRun');
    });
  });

  describe('dead letters extraction and rows', () => {
    const runEvents = (runRowsFixture as { events: Event[] }).events;

    it('extracts recorded dead-lettered run with reason and trigger event id from run-started', () => {
      const deadLetters = extractDeadLetters(runEvents);
      expect(deadLetters).toHaveLength(1);

      const item = deadLetters[0]!;
      expect(item.runId).toBe('01M3Y7MD5XWKPB08508JGNBW6M');
      // Trigger event id from matching run-started's provenance.runner.event_id
      expect(item.eventId).toBe('01M3Y7MD5FYFSDTTEM0WM64BCV');
      expect(item.slug).toBe('order-4500152');
      expect(item.reason).toBe('permanent');
      expect(item.error).toContain('harness "refusing" cannot run this task');
    });

    it('builds dead letter row with target page slug, reason description, and contextValue "deadLetter"', () => {
      const deadLetters = extractDeadLetters(runEvents);
      const rows = buildRunnerRows(newestStatusEvent, deadLetters, { admin: 'not-admin' });

      const deadLettersGroup = rows.find((r) => r.kind === 'deadLetters');
      expect(deadLettersGroup).toBeDefined();
      expect(deadLettersGroup?.children).toHaveLength(1);

      const row = deadLettersGroup?.children?.[0];
      expect(row?.kind).toBe('deadLetter');
      expect(row?.label).toBe('order-4500152');
      expect(row?.description).toContain('permanent');
      expect(row?.contextValue).toBe('deadLetter');
      expect(row?.runId).toBe('01M3Y7MD5XWKPB08508JGNBW6M');
      expect(row?.eventId).toBe('01M3Y7MD5FYFSDTTEM0WM64BCV');
    });

    it('extracts trigger event id also from single run-started fixture', () => {
      const startedEvent = runStartedDeadletterFixture as Event;
      const finishedEvent = runEvents.find((e) => e.title === 'run-finished')!;
      const deadLetters = extractDeadLetters([finishedEvent, startedEvent]);
      expect(deadLetters).toHaveLength(1);
      expect(deadLetters[0]?.eventId).toBe('01M3Y7MD5FYFSDTTEM0WM64BCV');
    });
  });

  describe('quotas row rules', () => {
    it('omits Quotas row for non-admin', () => {
      const rows = buildRunnerRows(newestStatusEvent, [], {
        admin: 'not-admin',
        quotas: { used: 10, limit: 100 },
      });
      expect(rows.find((r) => r.kind === 'quotas')).toBeUndefined();
    });

    it('omits Quotas row for unknown admin state', () => {
      const rows = buildRunnerRows(newestStatusEvent, [], {
        admin: 'unknown',
        quotas: { used: 10, limit: 100 },
      });
      expect(rows.find((r) => r.kind === 'quotas')).toBeUndefined();
    });

    it('omits Quotas row when admin quota tool returns error payload (like admin-quota-admin.json)', () => {
      const rows = buildRunnerRows(newestStatusEvent, [], {
        admin: 'admin',
        quotas: adminQuotaAdminFixture as Record<string, unknown>,
      });
      expect(rows.find((r) => r.kind === 'quotas')).toBeUndefined();
    });

    it('omits Quotas row when admin quota tool returns refusal payload (like admin-quota-human.json)', () => {
      const rows = buildRunnerRows(newestStatusEvent, [], {
        admin: 'admin',
        quotas: adminQuotaHumanFixture as Record<string, unknown>,
      });
      expect(rows.find((r) => r.kind === 'quotas')).toBeUndefined();
    });

    it('renders Quotas row for admin ONLY when real {used, limit} numbers are present', () => {
      const rows = buildRunnerRows(newestStatusEvent, [], {
        admin: 'admin',
        quotas: { used: 3, limit: 10 },
      });
      const quotaRow = rows.find((r) => r.kind === 'quotas');
      expect(quotaRow).toBeDefined();
      expect(quotaRow?.label).toBe('Quotas');
      expect(quotaRow?.description).toContain('3');
      expect(quotaRow?.description).toContain('10');
    });
  });

  describe('graceful handling of incomplete data', () => {
    it('a body missing optional fields does not throw', () => {
      expect(() => {
        const rows = buildRunnerRows({ at: '2026-10-02T12:00:00Z', body: '{}' }, [], {
          admin: 'not-admin',
        });
        expect(rows).toBeInstanceOf(Array);
        expect(rows.length).toBeGreaterThan(0);
      }).not.toThrow();

      expect(() => {
        const rows = buildRunnerRows(null, [], { admin: 'not-admin' });
        expect(rows).toBeInstanceOf(Array);
        expect(rows.length).toBeGreaterThan(0);
      }).not.toThrow();
    });
  });
});
