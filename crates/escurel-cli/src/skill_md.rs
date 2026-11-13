//! escurel skill page ↔ Anthropic-style `SKILL.md` (#825).
//!
//! Pure text conversion, no I/O: the `skill export` / `skill import` commands
//! own the gateway and the filesystem.
//!
//! The mapping is lossless in both directions for anything either side can
//! say:
//!
//! * escurel `id` ↔ SKILL.md `name`, `description` ↔ `description`, the body
//!   (the skill's instructions) ↔ the body.
//! * Every other escurel key (`acl`, `schedule`, `agent`, `fields`, …) rides
//!   in `metadata.escurel`, the namespace SKILL.md leaves to its producers.
//! * SKILL.md keys escurel has no use for (`license`, `allowed-tools`, other
//!   `metadata` entries) ride in the page's `skill_md:` key and come back out
//!   on export.
//! * Wikilinks stay in the body as written — they read fine as text — and
//!   the export appends a fenced "escurel references" section telling an
//!   external agent to look each one up with the MCP `resolve` tool. The
//!   import strips exactly that section again.

use anyhow::{Result, anyhow, bail};
use escurel_md::{PageKind, YamlMapping, YamlValue};

/// The file an Anthropic skill directory is named after.
pub const SKILL_FILE: &str = "SKILL.md";

/// Brackets the section [`export`] appends. HTML comments render as nothing,
/// and they make the section unambiguous to find on the way back in.
const HINTS_OPEN: &str = "<!-- escurel:resolve-hints -->";
const HINTS_CLOSE: &str = "<!-- /escurel:resolve-hints -->";

/// Page keys the SKILL.md frontmatter already carries in its own words.
const MAPPED: &[&str] = &["kind", "id", "description"];
/// The page key holding SKILL.md keys escurel does not interpret.
const PASSTHROUGH: &str = "skill_md";
/// Where a page sits, stamped by the pack importer: not part of the skill.
const PLACEMENT: &[&str] = &["layer"];

/// The page id a skill's page lives at.
pub fn skill_page_id(skill_id: &str) -> String {
    format!("markdown/skills/{skill_id}.md")
}

/// A skill page rendered as SKILL.md.
pub struct Exported {
    /// The SKILL.md `name` — the skill id, and the directory to write it in.
    pub name: String,
    pub skill_md: String,
}

/// Render an escurel skill page's stored markdown as a SKILL.md document.
///
/// A page without a `description` gets its `summary` (or a stock line) as
/// the SKILL.md description, which SKILL.md requires; importing it back then
/// adds that description to the page — the one direction that is not
/// lossless.
pub fn export(page: &str) -> Result<Exported> {
    let parsed = escurel_md::parse(page)?;
    if parsed.frontmatter.page_kind != PageKind::Skill {
        bail!("not a skill page (`kind: instance`): only skills export as SKILL.md");
    }
    let fields = parsed.frontmatter.fields;
    let name = str_field(&fields, "id")
        .ok_or_else(|| anyhow!("skill page has no `id:`"))?
        .to_owned();
    // The name becomes a directory: a stored id that is not a plain slug is not written out.
    if !is_skill_dir_name(&name) {
        bail!(
            "skill id `{}` cannot name a SKILL.md directory",
            name.escape_default()
        );
    }
    let description = str_field(&fields, "description")
        .or_else(|| str_field(&fields, "summary"))
        .map_or_else(|| format!("escurel skill `{name}`"), str::to_owned);

    let mut escurel = YamlMapping::new();
    let mut passthrough = YamlMapping::new();
    for (key, value) in fields {
        match key.as_str() {
            Some(k) if MAPPED.contains(&k) || PLACEMENT.contains(&k) => {}
            Some(PASSTHROUGH) => match value {
                YamlValue::Mapping(m) => passthrough = m,
                other => {
                    escurel.insert(key, other);
                }
            },
            _ => {
                escurel.insert(key, value);
            }
        }
    }

    let mut out = YamlMapping::new();
    out.insert(key("name"), YamlValue::String(name.clone()));
    out.insert(key("description"), YamlValue::String(description));
    let mut metadata = match passthrough.remove("metadata") {
        Some(YamlValue::Mapping(m)) => m,
        Some(other) => bail!("`skill_md.metadata` must be a mapping, found {other:?}"),
        None => YamlMapping::new(),
    };
    out.extend(passthrough);
    if !escurel.is_empty() {
        metadata.insert(key("escurel"), YamlValue::Mapping(escurel));
    }
    if !metadata.is_empty() {
        out.insert(key("metadata"), YamlValue::Mapping(metadata));
    }

    let body = format!("{}{}", parsed.body, resolve_hints(parsed.body));
    Ok(Exported {
        skill_md: escurel_md::render_page(&out, &body)?,
        name,
    })
}

/// A SKILL.md document turned into an escurel skill page.
pub struct Imported {
    pub skill_id: String,
    /// The whole page markdown (`kind: skill`).
    pub page: String,
}

/// Turn a SKILL.md document into an escurel skill page. Refuses a `name` that
/// is not a valid skill id: it becomes part of the page id.
pub fn import(skill_md: &str) -> Result<Imported> {
    let (fields, body) = escurel_md::split_frontmatter(skill_md)
        .map_err(|e| anyhow!("{SKILL_FILE} frontmatter: {e}"))?;
    let name = match fields.get("name") {
        Some(YamlValue::String(s)) => s.clone(),
        Some(other) => bail!("{SKILL_FILE} `name` must be a string, found {other:?}"),
        None => bail!("{SKILL_FILE} has no `name:` — it names the skill"),
    };
    if !is_skill_dir_name(&name) {
        bail!(
            "{SKILL_FILE} `name` `{}` is not a valid escurel skill id \
             (letters, digits, `_`, `-`, `.`, not starting with `.`; at most 128 characters)",
            name.escape_default()
        );
    }

    let mut page = YamlMapping::new();
    page.insert(key("kind"), YamlValue::String("skill".to_owned()));
    page.insert(key("id"), YamlValue::String(name.clone()));
    let mut passthrough = YamlMapping::new();
    let mut escurel = YamlMapping::new();
    for (k, value) in fields {
        match k.as_str() {
            Some("name") => {}
            Some("description") => {
                page.insert(k, value);
            }
            Some("metadata") => {
                let YamlValue::Mapping(mut metadata) = value else {
                    bail!("{SKILL_FILE} `metadata` must be a mapping");
                };
                match metadata.remove("escurel") {
                    Some(YamlValue::Mapping(m)) => escurel = m,
                    Some(_) => bail!("{SKILL_FILE} `metadata.escurel` must be a mapping"),
                    None => {}
                }
                if !metadata.is_empty() {
                    passthrough.insert(k, YamlValue::Mapping(metadata));
                }
            }
            _ => {
                passthrough.insert(k, value);
            }
        }
    }
    for (k, value) in escurel {
        // The SKILL.md's own `name`/`description` are the authority for these;
        // a page's placement is the importer's, never the bundle's.
        if k.as_str()
            .is_some_and(|k| MAPPED.contains(&k) || PLACEMENT.contains(&k))
        {
            continue;
        }
        page.insert(k, value);
    }
    if !passthrough.is_empty() {
        page.insert(key(PASSTHROUGH), YamlValue::Mapping(passthrough));
    }

    Ok(Imported {
        page: escurel_md::render_page(&page, &strip_resolve_hints(body))?,
        skill_id: name,
    })
}

/// A skill id that is also safe as one path segment: the gateway's skill-id rule, minus a leading
/// `.` (which would let `..` name the parent directory, or hide the folder).
pub fn is_skill_dir_name(name: &str) -> bool {
    escurel_types::is_safe_skill_id(name) && !name.starts_with('.')
}

/// The section [`export`] appends: one `resolve` hint per distinct wikilink,
/// in document order. Empty when the body cites nothing.
fn resolve_hints(body: &str) -> String {
    let mut links: Vec<String> = Vec::new();
    for w in escurel_md::wikilink::parse_wikilinks(body) {
        let link = wikilink_text(&w);
        if !links.contains(&link) {
            links.push(link);
        }
    }
    if links.is_empty() {
        return String::new();
    }
    let items: String = links.iter().map(|l| format!("- `{l}`\n")).collect();
    format!(
        "\n{HINTS_OPEN}\n## escurel references\n\n\
         The `[[…]]` links above name pages in an escurel knowledge base. Look one up with \
         the escurel MCP `resolve` tool (`{{\"wikilink\": \"[[skill::id]]\"}}`), then `expand` \
         the page it returns:\n\n{items}{HINTS_CLOSE}\n"
    )
}

/// The body without the section [`resolve_hints`] appended.
fn strip_resolve_hints(body: &str) -> String {
    let open = format!("\n{HINTS_OPEN}\n");
    let Some(start) = body.rfind(&open) else {
        return body.to_owned();
    };
    let Some(close) = body[start..].find(HINTS_CLOSE) else {
        return body.to_owned();
    };
    let rest = &body[start + close + HINTS_CLOSE.len()..];
    format!(
        "{}{}",
        &body[..start],
        rest.strip_prefix('\n').unwrap_or(rest)
    )
}

/// `[[skill::id#anchor@version]]` — the link without its display alias, which
/// is what `resolve` takes.
fn wikilink_text(w: &escurel_md::wikilink::WikilinkParsed) -> String {
    let mut s = String::from("[[");
    if let Some(skill) = &w.skill {
        s.push_str(skill);
        s.push_str("::");
    }
    s.push_str(w.id.as_deref().unwrap_or_default());
    if let Some(anchor) = &w.anchor {
        s.push('#');
        s.push_str(anchor);
    }
    if let Some(version) = &w.version {
        s.push('@');
        s.push_str(version);
    }
    s.push_str("]]");
    s
}

fn key(k: &str) -> YamlValue {
    YamlValue::String(k.to_owned())
}

fn str_field<'a>(fields: &'a YamlMapping, k: &str) -> Option<&'a str> {
    fields.get(k).and_then(YamlValue::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SKILL: &str = "---\n\
kind: skill\n\
id: triage\n\
description: Triage an inbound support ticket.\n\
autonomy: review\n\
schedule: \"0 7 * * 1-5\"\n\
acl:\n  read: [support]\n\
fields:\n  priority: {kind: enum, values: [low, high]}\n\
---\n\
# triage\n\nRead the ticket, then check [[customer::acme|Acme]] and [[playbook#escalate]].\n\
Again: [[customer::acme]].\n";

    fn fields_of(page: &str) -> YamlMapping {
        escurel_md::split_frontmatter(page).unwrap().0
    }

    /// The fixture pair the VS Code port (`editors/vscode/src/skills/skillMd.ts`) is tested
    /// against too, so the two renderings cannot drift apart unnoticed.
    const GOLDEN_PAGE: &str = include_str!("../tests/fixtures/skill-md/page.md");
    const GOLDEN_SKILL_MD: &str = include_str!("../tests/fixtures/skill-md/SKILL.md");

    #[test]
    fn export_matches_the_shared_golden_fixture() {
        let out = export(GOLDEN_PAGE).unwrap();
        assert_eq!(out.name, "triage");
        assert_eq!(out.skill_md, GOLDEN_SKILL_MD, "actual:\n{}", out.skill_md);
        assert_eq!(import(GOLDEN_SKILL_MD).unwrap().page, GOLDEN_PAGE);
    }

    #[test]
    fn export_maps_id_and_description_and_namespaces_the_rest() {
        let out = export(SKILL).unwrap();
        assert_eq!(out.name, "triage");
        let fm = fields_of(&out.skill_md);
        assert_eq!(str_field(&fm, "name"), Some("triage"));
        assert_eq!(
            str_field(&fm, "description"),
            Some("Triage an inbound support ticket.")
        );
        assert!(fm.get("kind").is_none() && fm.get("id").is_none());
        let escurel = &fm["metadata"]["escurel"];
        assert_eq!(escurel["autonomy"].as_str(), Some("review"));
        assert_eq!(escurel["schedule"].as_str(), Some("0 7 * * 1-5"));
        assert!(escurel["acl"]["read"].is_sequence());
        assert!(escurel["fields"]["priority"].is_mapping());
    }

    #[test]
    fn export_keeps_links_readable_and_lists_each_once_as_a_resolve_hint() {
        let md = export(SKILL).unwrap().skill_md;
        // The prose is untouched, alias and all.
        assert!(md.contains("check [[customer::acme|Acme]] and [[playbook#escalate]]."));
        let hints = &md[md.find(HINTS_OPEN).expect("hint section")..];
        assert!(hints.contains("`resolve`"));
        assert_eq!(hints.matches("- `[[customer::acme]]`").count(), 1);
        assert!(hints.contains("- `[[playbook#escalate]]`"));
    }

    #[test]
    fn a_body_without_links_gets_no_hint_section() {
        let page = "---\nkind: skill\nid: plain\ndescription: d\n---\n# plain\n";
        let md = export(page).unwrap().skill_md;
        assert!(!md.contains(HINTS_OPEN));
        // Nothing but name + description: no empty metadata block either.
        assert!(!md.contains("metadata"));
    }

    #[test]
    fn round_trip_is_lossless() {
        let back = import(&export(SKILL).unwrap().skill_md).unwrap();
        assert_eq!(back.skill_id, "triage");
        let (orig_fm, orig_body) = escurel_md::split_frontmatter(SKILL).unwrap();
        let (back_fm, back_body) = escurel_md::split_frontmatter(&back.page).unwrap();
        assert_eq!(back_body, orig_body);
        // Order-insensitive: a page's keys are a mapping, not a list.
        let sorted = |m: YamlMapping| {
            let mut v: Vec<_> = m.into_iter().map(|(k, v)| (format!("{k:?}"), v)).collect();
            v.sort_by(|a, b| a.0.cmp(&b.0));
            v
        };
        assert_eq!(sorted(back_fm), sorted(orig_fm));
    }

    #[test]
    fn round_trip_of_a_body_without_trailing_newline() {
        let page = "---\nkind: skill\nid: t\ndescription: d\n---\nsee [[x::y]]";
        let back = import(&export(page).unwrap().skill_md).unwrap();
        assert_eq!(
            escurel_md::split_frontmatter(&back.page).unwrap().1,
            "see [[x::y]]"
        );
    }

    #[test]
    fn an_anthropic_skill_imports_as_a_minimal_skill_and_keeps_its_own_keys() {
        let md = "---\nname: pdf-tools\ndescription: Fill and merge PDFs.\nlicense: MIT\n\
                  metadata:\n  author: someone\n---\n# PDF tools\n\nUse pypdf.\n";
        let page = import(md).unwrap().page;
        let parsed = escurel_md::parse(&page).expect("a valid escurel page");
        assert_eq!(parsed.frontmatter.page_kind, PageKind::Skill);
        let fm = parsed.frontmatter.fields;
        assert_eq!(str_field(&fm, "id"), Some("pdf-tools"));
        assert_eq!(str_field(&fm, "description"), Some("Fill and merge PDFs."));
        assert_eq!(fm["skill_md"]["license"].as_str(), Some("MIT"));
        assert_eq!(
            fm["skill_md"]["metadata"]["author"].as_str(),
            Some("someone")
        );
        assert_eq!(parsed.body, "# PDF tools\n\nUse pypdf.\n");
        // ... and they come back out where they were.
        let again = fields_of(&export(&page).unwrap().skill_md);
        assert_eq!(again["license"].as_str(), Some("MIT"));
        assert_eq!(again["metadata"]["author"].as_str(), Some("someone"));
        assert!(again.get("skill_md").is_none());
    }

    #[test]
    fn import_refuses_a_name_that_is_not_a_skill_id() {
        for bad in ["../etc", "..", ".hidden", "has space", "a/b", ""] {
            let md = format!("---\nname: \"{bad}\"\ndescription: d\n---\nx\n");
            let err = import(&md).err().expect("refused").to_string();
            assert!(err.contains("not a valid escurel skill id"), "{bad}: {err}");
        }
        let err = import("---\ndescription: d\n---\nx\n")
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("no `name:`"), "{err}");
    }

    #[test]
    fn import_never_takes_kind_id_or_layer_from_metadata() {
        let md = "---\nname: t\ndescription: d\nmetadata:\n  escurel:\n    kind: instance\n    \
                  id: other\n    layer: base@p@v1\n    autonomy: auto\n---\nx\n";
        let fm = fields_of(&import(md).unwrap().page);
        assert_eq!(str_field(&fm, "kind"), Some("skill"));
        assert_eq!(str_field(&fm, "id"), Some("t"));
        assert!(fm.get("layer").is_none());
        assert_eq!(str_field(&fm, "autonomy"), Some("auto"));
    }

    #[test]
    fn export_drops_the_pack_layer_and_refuses_an_instance() {
        let base = "---\nlayer: base@crm@v2\nkind: skill\nid: c\ndescription: d\n---\nx\n";
        assert!(!export(base).unwrap().skill_md.contains("layer"));
        let inst = "---\nkind: instance\nskill: c\nid: a\n---\nx\n";
        assert!(export(inst).is_err());
        let dots = "---\nkind: skill\nid: ..\ndescription: d\n---\nx\n";
        assert!(
            export(dots).is_err(),
            "`..` must not name the output directory"
        );
    }
}
