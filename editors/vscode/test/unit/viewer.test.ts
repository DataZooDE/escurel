import { describe, expect, it } from 'vitest';
import { parseViewer } from '../../src/shared/viewer';

// A skill page may declare `viewer: {report, param}`: the report skill a renderer (Peacock) uses to
// draw its records. It is a relation between skills, not a record, so a record page names it.
describe('parseViewer', () => {
  it('reads the report a skill names as its viewer', () => {
    expect(parseViewer({ viewer: { report: 'supplier-risk-report', param: 'analysis' } })).toEqual({
      report: 'supplier-risk-report',
    });
  });
  it('says nothing when there is no viewer, or it is not shaped like one', () => {
    expect(parseViewer({})).toBeUndefined();
    expect(parseViewer(undefined)).toBeUndefined();
    expect(parseViewer({ viewer: 'x' })).toBeUndefined();
    expect(parseViewer({ viewer: { report: 7 } })).toBeUndefined();
    expect(parseViewer({ viewer: { report: '' } })).toBeUndefined();
  });
  it('refuses a report name that is not a skill id, so a page cannot name anything else', () => {
    expect(parseViewer({ viewer: { report: '../../etc/passwd' } })).toBeUndefined();
    expect(parseViewer({ viewer: { report: 'a b' } })).toBeUndefined();
  });
});
