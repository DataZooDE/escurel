import { describe, expect, it } from 'vitest';
import { exposedApi } from '../../src/shared/apiExposure';

// `activate()` used to return the whole service graph, token store included, to ANY installed
// extension (`getExtension('datazoo.escurel').exports`). The test and demo harnesses need it; a real
// install must not hand it out.
describe('exposedApi', () => {
  const api = { services: { auth: { refresher: 'the token store' } } };

  it('hands nothing to other extensions in a production install', () => {
    expect(exposedApi(true, api)).toBeUndefined();
  });

  it('hands the API to the test and demo harnesses', () => {
    expect(exposedApi(false, api)).toBe(api);
  });
});
