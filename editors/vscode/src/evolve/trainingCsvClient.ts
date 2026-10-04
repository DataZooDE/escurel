import type { TokenRefresher } from '../auth/refresher';
import { evolveOrigin } from './holdoutClient';

export type PrivateTrainingTool = 'evolve_prepare_training_csv' | 'evolve_training_csv_draft';

function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value))
    throw new Error('Evolve returned an invalid training CSV receipt.');
  return value as Record<string, unknown>;
}

/** Direct owner-authenticated channel. Never forward these bodies to an Escurel event or chat. */
export async function callPrivateTrainingTool(
  endpoint: string, refresher: TokenRefresher, tool: PrivateTrainingTool,
  body: Record<string, unknown>,
): Promise<Record<string, unknown>> {
  const origin = evolveOrigin(endpoint);
  let lastStatus = 0;
  for (let attempt = 0; attempt < 3; attempt++) {
    const token = attempt === 1 && lastStatus === 401
      ? await refresher.invalidate() : await refresher.get();
    if (!token) throw new Error('Sign in with an OIDC token accepted by Evolve.');
    let response: Response;
    try {
      response = await fetch(origin + '/', {
        method: 'POST', redirect: 'error', signal: AbortSignal.timeout(30_000),
        headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json',
          'X-Triton-Tool': tool },
        body: JSON.stringify(body),
      });
    } catch {
      if (attempt < 2) continue;
      throw new Error('Evolve did not confirm preparation. Retry the same source ID and files; exact retries return the frozen receipt.');
    }
    lastStatus = response.status;
    if (response.status === 401 && attempt === 0) continue;
    if (response.status === 503 && attempt < 2) continue;
    if (response.status === 409)
      throw new Error('This source ID is already frozen with different files. Use a new ID for the changed extract.');
    if (response.status === 403)
      throw new Error('This source belongs to another signed-in owner. Use the original owner or a new source ID.');
    if (response.status === 422)
      throw new Error('Evolve rejected the CSV schema or measurements. Fix the selected files and retry the same ID.');
    if (response.status === 401)
      throw new Error('Evolve rejected the OIDC token. Sign in again and check its audience.');
    if (!response.ok)
      throw new Error(`Evolve rejected ${tool} (HTTP ${response.status}). Retry the same files and ID if the result is uncertain.`);
    return object(await response.json());
  }
  throw new Error('Evolve did not confirm preparation. Retry the same source ID and files.');
}
