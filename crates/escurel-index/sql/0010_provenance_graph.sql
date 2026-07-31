-- 0010 — provenance graph (ADR-0010).
--
-- `resolved_links` is a DERIVED read surface over the `links`/`pages`
-- tables — the single schema object the bounded graph-query tools
-- (provenance_ancestry / _path / expectation_drift / abandoned_paths)
-- traverse. It is a VIEW, not a table: nothing is stored, it is rebuilt
-- (CREATE OR REPLACE) on every connection, and it stays fully derivable
-- from canonical markdown per ADR-0001.
--
-- Two things the raw `links` table can't give a graph query directly:
--
--   1. Resolved endpoints. `links.dst_page` is the target SLUG, not a
--      page_id, and links may dangle (point at a page that doesn't
--      exist). The INNER JOIN to `pages` on (slug, skill) resolves the
--      destination to a real `dst_page_id` and DROPS dangling links —
--      they cannot appear in a path.
--
--   2. The relation KIND. A frontmatter relation `derived_from:
--      [[analysis::x]]` is stored with `src_field = 'frontmatter.derived_from'`;
--      stripping the `frontmatter.` prefix exposes `relation =
--      'derived_from'`. Body links (src_field NULL) keep a NULL relation.
--
-- `src_at_ts` / `src_scenario` / `dst_at_ts` are exposed so the query
-- layer can enforce the same time-travel (`as_of`) gate `neighbours`
-- uses and compare timestamps (expectation_drift). Scenario overlays are
-- a v2 refinement (REQ: base-graph first) — the destination join is
-- pinned to base pages (`dst.scenario IS NULL`) and callers filter the
-- source to base (`src_scenario IS NULL`), matching `neighbours`'
-- default.
CREATE OR REPLACE VIEW resolved_links AS
SELECT
    l.src_page                                          AS src_page_id,
    dst.page_id                                         AS dst_page_id,
    CASE
        WHEN l.src_field LIKE 'frontmatter.%'
            THEN substr(l.src_field, length('frontmatter.') + 1)
        ELSE l.src_field
    END                                                 AS relation,
    l.link_skill                                        AS dst_skill,
    src.skill                                           AS src_skill,
    l.link_version                                      AS link_version,
    l.dst_anchor                                        AS dst_anchor,
    src.at_ts                                           AS src_at_ts,
    src.scenario                                        AS src_scenario,
    dst.at_ts                                           AS dst_at_ts
FROM links l
JOIN pages src ON src.page_id = l.src_page
JOIN pages dst ON dst.slug = l.dst_page
             AND dst.skill = l.link_skill
             AND dst.scenario IS NULL;
