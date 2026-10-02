import { describe, expect, it } from 'vitest';
import { EscurelError } from '../../src/client/errors';
import { describeControlRefusal } from '../../src/runs/controlResult';
import recorded from './fixtures/runner/control-refusal-non-admin-pause.json';

describe('control refusal', () => {
  it('does not reveal whether a denied run exists', () => {
    const err = EscurelError.fromRpc(
      'capture_event',
      recorded.error.code,
      recorded.error.message,
      recorded.error.data,
    );
    expect(describeControlRefusal(err)).toBe('No such run, or not yours to control.');
  });

  it('shows malformed request details and other refusals', () => {
    expect(describeControlRefusal(new EscurelError('invalid_params', 'Bad event ID'))).toBe(
      'Bad event ID',
    );
    expect(describeControlRefusal(new Error('offline'))).toBe('offline');
  });
});
