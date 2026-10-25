/**
 * What `activate()` returns to OTHER extensions.
 *
 * Two layers. The public API (`src/api`, versioned, docs/design/extension-api.md) is handed out in every
 * mode: it holds no token and checks what a caller passes. The internals are the whole service graph,
 * the token store included, so any installed extension could read the bearer
 * (`getExtension('datazoo.escurel').exports.services.auth`) or set its own. The integration suite, the
 * e2e tests and the demo bootstrap need them; a production install does not hand them out.
 */
export function exposedApi<P extends object, I extends object>(
  isProduction: boolean,
  publicApi: P,
  internals: I,
): P | (P & I) {
  return isProduction ? publicApi : { ...publicApi, ...internals };
}
