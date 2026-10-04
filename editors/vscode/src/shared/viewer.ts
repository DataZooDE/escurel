/** A skill id as it appears in `viewer:`: the same slug shape a skill page's file name has. */
const SKILL_ID = /^[A-Za-z0-9][A-Za-z0-9_.-]*$/;

/**
 * The report skill a skill page names as its viewer (`viewer: {report, param}`), or undefined.
 *
 * A report is a relation between skills, not a record: it draws the records of a skill (a chart,
 * for example) and is never run, so it can never be a node of a thread. A record page names it so
 * a person can find it. The name is checked to be a plain skill id: a page cannot make the
 * extension open anything else through this field.
 */
export function parseViewer(frontmatter: unknown): { report: string } | undefined {
  if (typeof frontmatter !== 'object' || frontmatter === null) return undefined;
  const viewer = (frontmatter as { viewer?: unknown }).viewer;
  if (typeof viewer !== 'object' || viewer === null) return undefined;
  const report = (viewer as { report?: unknown }).report;
  return typeof report === 'string' && SKILL_ID.test(report) ? { report } : undefined;
}
