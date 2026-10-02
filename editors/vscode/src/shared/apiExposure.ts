/**
 * What `activate()` returns to OTHER extensions.
 *
 * The returned object is the whole service graph, the token store included, so any installed
 * extension could read the bearer (`getExtension('datazoo.escurel').exports.services.auth`) or set
 * its own. The integration suite, the e2e tests and the demo bootstrap need it; a production install
 * does not hand it out.
 */
export function exposedApi<T>(isProduction: boolean, api: T): T | undefined {
  return isProduction ? undefined : api;
}
