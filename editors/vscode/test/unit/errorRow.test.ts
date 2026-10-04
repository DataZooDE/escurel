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

  it('keeps a raw technical error out of the label: the label is a sentence, the tooltip has the detail', () => {
    expect(errorRowSpec("Couldn't load instances.", 'rpc: MCP error -32603: list_in…')).toEqual({
      label: "Couldn't load instances.",
      tooltip: "Couldn't load instances.\n\nrpc: MCP error -32603: list_in…\n\nClick to try again.",
      command: 'escurel.refresh',
    });
  });
});
