import type { EscurelClient, UpdatePageResponse } from '../client';

/** Save = `update_page` guarded by the hash the page was read at (SPEC §1). A stale hash throws `EscurelError{kind: 'conflict'}` with the head. */
export function writeSkill(
  client: EscurelClient,
  pageId: string,
  content: string,
  baseSha256: string | undefined,
): Promise<UpdatePageResponse> {
  return client.updatePage({
    page_id: pageId,
    content,
    ...(baseSha256 ? { base_sha256: baseSha256 } : {}),
    provenance: { source: 'vscode' },
  });
}
