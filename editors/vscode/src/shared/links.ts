/** Where a link REALLY goes, and whether its words claim a different place (the classic phishing shape). */

const ADDRESS_LIKE = /^(?:https?:\/\/)?(?:www\.)?([a-z0-9-]+(?:\.[a-z0-9-]+)+)(?:[/:?#]|$)/i;

export function hostOf(href: string): string {
  try {
    return new URL(href).host.replace(/^www\./i, '').toLowerCase();
  } catch {
    return '';
  }
}

/** True if `label` itself reads as an address (`https://sap.example/login`) that is not where `href` goes. */
export function labelClaimsOtherSite(label: string, href: string): boolean {
  if (!/^https?:/i.test(href)) return false;
  const claimed = ADDRESS_LIKE.exec(label.trim())?.[1]
    ?.toLowerCase()
    .replace(/^www\./, '');
  if (!claimed) return false;
  return claimed !== hostOf(href);
}
