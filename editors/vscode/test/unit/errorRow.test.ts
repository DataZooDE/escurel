import { describe, expect, it } from 'vitest';
import { errorRowSpec } from '../../src/views/errorRow';

// A tree that failed to load used to show the raw error and nothing to do about it. The row now
// says what happened, says what clicking does, and clicking it tries again.
describe('errorRowSpec', () => {
  it('shows the error and retries the load when clicked', () => {
    expect(errorRowSpec('connect ECONNREFUSED 127.0.0.1:8080')).toEqual({
      label: 'connect ECONNREFUSED 127.0.0.1:8080',
      tooltip: 'connect ECONNREFUSED 127.0.0.1:8080\n\nClick to try again.',
      command: 'escurel.refresh',
    });
  });

  it('never has an empty label', () => {
    expect(errorRowSpec('').label).toBe('Could not load this view.');
  });
});
