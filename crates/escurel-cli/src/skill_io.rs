//! `skill export` / `skill import`: the gateway and filesystem half of the
//! SKILL.md converter in [`escurel_md::skill_md`].

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use escurel_client::{Client, CreateDraftRequest, ExpandRequest};
use serde_json::{Value, json};

use crate::agent::{ExportFormat, SkillExportArgs, SkillImportArgs, draft_json};
use escurel_md::skill_md::{self, SKILL_FILE, skill_page_id};

/// The bundle `skill export --pack` writes.
const PACK_FILE: &str = "escurel-skills.tar.gz";

pub async fn export(client: &Client, a: SkillExportArgs) -> Result<Value> {
    let ExportFormat::SkillMd = a.export_as;
    let mut exported = Vec::with_capacity(a.skills.len());
    for skill in &a.skills {
        let (content, _) = stored_skill(client, skill)
            .await?
            .with_context(|| format!("no skill `{skill}` in this tenant"))?;
        exported.push(
            skill_md::export(&content).with_context(|| format!("exporting skill `{skill}`"))?,
        );
    }
    let files: Vec<(PathBuf, Vec<u8>)> = if a.pack {
        // Sorted, as the deterministic tarball expects: re-exporting unchanged
        // skills yields byte-identical bundles.
        let mut entries: Vec<(String, String)> = exported
            .into_iter()
            .map(|e| (format!("{}/{SKILL_FILE}", e.name), e.skill_md))
            .collect();
        entries.sort_by(|x, y| x.0.cmp(&y.0));
        let tarball = escurel_types::pack::build_tarball(&entries)?;
        vec![(a.out.join(PACK_FILE), tarball)]
    } else {
        exported
            .into_iter()
            .map(|e| {
                (
                    a.out.join(&e.name).join(SKILL_FILE),
                    e.skill_md.into_bytes(),
                )
            })
            .collect()
    };

    // Decide every file before writing any: a refusal leaves the directory
    // exactly as it was, not half-exported.
    let mut differing = Vec::new();
    let mut unchanged = Vec::new();
    for (path, bytes) in &files {
        match std::fs::read(path) {
            Ok(existing) if existing == *bytes => unchanged.push(path.clone()),
            Ok(_) => differing.push(path.display().to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }
    if !differing.is_empty() && !a.force {
        bail!(
            "refusing to overwrite {} (different content: a local edit?); re-run with --force \
             to replace {}",
            differing.join(", "),
            if differing.len() == 1 { "it" } else { "them" }
        );
    }
    let mut written = Vec::with_capacity(files.len());
    for (path, bytes) in files {
        if unchanged.contains(&path) {
            continue;
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        write(&path, &bytes)?;
        written.push(path);
    }
    let shown = |paths: &[PathBuf]| {
        paths
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
    };
    Ok(json!({
        "skills": a.skills,
        "written": shown(&written),
        "unchanged": shown(&unchanged),
    }))
}

pub async fn import(client: &Client, a: SkillImportArgs) -> Result<Value> {
    let file = a.dir.join(SKILL_FILE);
    let text = std::fs::read_to_string(&file).with_context(|| {
        format!(
            "reading {} (a skill directory holds a {SKILL_FILE})",
            file.display()
        )
    })?;
    let imported = skill_md::import(&text)?;
    let extra_files = extra_files(&a.dir)?;

    // An existing skill is drafted against what is stored now, so promoting
    // the draft cannot overwrite an edit that lands in between.
    let stored = stored_skill(client, &imported.skill_id).await?;
    let removed = stored
        .as_ref()
        .map(|(page, _)| skill_md::removed_authority_keys(page, &imported.page))
        .unwrap_or_default();
    let base_sha256 = stored.map(|(_, sha)| sha).unwrap_or_default();
    let target_page_id = skill_page_id(&imported.skill_id);
    let resp = client
        .create_draft(CreateDraftRequest {
            target_page_id: target_page_id.clone(),
            content: imported.page,
            base_sha256: Some(base_sha256),
            event_id: String::new(),
        })
        .await?;
    let mut out = json!({
        "ok": resp.ok,
        "skill": imported.skill_id,
        "target_page_id": target_page_id,
        "draft": resp.draft.map(|d| draft_json(d, true)),
        "issues": resp.issues,
    });
    if !imported.authority_keys.is_empty() {
        // A third-party bundle can grant reads, name an agent or schedule runs
        // through `metadata.escurel`; whoever promotes the draft should know.
        let keys = imported.authority_keys.join(", ");
        out["authority_keys"] = json!(imported.authority_keys);
        out["authority_note"] = json!(format!(
            "this draft sets {keys}: review them before promoting it; only an admin, or a member \
             of the stored skill's `acl.update`, may promote a change to them"
        ));
        eprintln!(
            "warning: the imported skill sets {keys}; review them before promoting the draft \
             (an admin's to promote, or a member of the stored skill's acl.update)"
        );
    }
    if !removed.is_empty() {
        // The stored skill is governed by keys this bundle does not carry: a
        // plain third-party SKILL.md promoted over it would drop its read ACL,
        // its agent binding, its schedule — silently.
        let keys = removed.join(", ");
        out["removed_authority_keys"] = json!(removed);
        out["removed_authority_note"] = json!(format!(
            "the stored skill sets {keys} and this draft does not: promoting it removes them"
        ));
        eprintln!(
            "warning: the stored skill `{}` sets {keys}; this draft does not, so promoting it \
             removes them",
            imported.skill_id
        );
    }
    if !extra_files.is_empty() {
        // escurel keeps a skill as one page; there is no place for a skill's
        // scripts or assets, so they are reported rather than silently dropped.
        out["extra_files"] = json!(extra_files);
        out["extra_files_note"] =
            json!("not uploaded: an escurel skill is one page; keep these files with the agent");
    }
    Ok(out)
}

/// The stored markdown of a skill and its `content_sha256`, or `None` when the
/// tenant has no such skill.
async fn stored_skill(client: &Client, skill: &str) -> Result<Option<(String, String)>> {
    if !skill_md::is_skill_dir_name(skill) {
        bail!("`{}` is not a valid skill id", skill.escape_default());
    }
    let resp = client
        .expand(ExpandRequest {
            page_id: skill_page_id(skill),
            raw: true,
            ..Default::default()
        })
        .await?;
    if resp.page.is_none() {
        return Ok(None);
    }
    let content = resp
        .content
        .context("the gateway returned no stored markdown for `expand {raw: true}`")?;
    Ok(Some((content, resp.content_sha256.unwrap_or_default())))
}

/// Every file under `dir` except the top-level SKILL.md, relative and sorted.
fn extra_files(dir: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).with_context(|| format!("listing {}", d.display()))? {
            let entry = entry?;
            let path = entry.path();
            // `file_type` does not follow symlinks, so a link cycle cannot loop the walk.
            if entry.file_type()?.is_dir() {
                stack.push(path);
            } else if path != dir.join(SKILL_FILE) {
                let rel = path.strip_prefix(dir).unwrap_or(&path);
                out.push(rel.display().to_string());
            }
        }
    }
    out.sort();
    Ok(out)
}

fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))
}
