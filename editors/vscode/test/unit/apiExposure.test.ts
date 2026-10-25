import { describe, expect, it } from 'vitest';
import { exposedApi } from '../../src/shared/apiExposure';

// `activate()` used to return the whole service graph, token store included, to ANY installed
// extension (`getExtension('datazoo.escurel').exports`). The test and demo harnesses need it; a real
// install must not hand it out. The versioned public API (no token in it) is handed out in every mode.
describe('exposedApi', () => {
  const pub = { apiVersion: 1 };
  const internals = { services: { auth: { refresher: 'the token store' } } };

  it('hands only the public API to other extensions in a production install', () => {
    const api = exposedApi(true, pub, internals);
    expect(api).toBe(pub);
    expect(api).not.toHaveProperty('services');
  });

  it('adds the internals for the test and demo harnesses', () => {
    const api = exposedApi(false, pub, internals);
    expect(api).toMatchObject({ apiVersion: 1, services: internals.services });
  });
});
