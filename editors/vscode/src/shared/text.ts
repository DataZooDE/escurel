/**
 * Renders a count alongside its singular or plural noun so the surfaces never
 * speak of "1 drafts" or "1 fields". Lives here because both the review and the
 * awaiting surfaces need it, and neither should depend on the other.
 */
export function pluralise(count: number, singular: string, plural = `${singular}s`): string {
  return `${count} ${count === 1 ? singular : plural}`;
}
