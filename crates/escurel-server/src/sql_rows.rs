//! The write-back guard for a row of a DATABASE-backed `rows` skill (the counterpart of
//! [`crate::remote_rows::write_rejection`]).
//!
//! A `write_back` intent is a human-gated instruction to the SOURCE database: it may only travel in a
//! DRAFT (the promote hook is the gate), only for a source that can be written at all, and only for the
//! columns the skill declares writable. Everything else about writing to a row page (the linked
//! markdown, `backend_ref`, source columns in the frontmatter) is the index's `rows_write_rejection`.

use escurel_index::Indexer;
use escurel_index::backend::rows::{RowsWriteRejection, split_instance_page_id};

/// `None` when the page is not a database `rows` page, carries no intent, or the intent is fine.
pub(crate) async fn write_rejection(
    indexer: &Indexer,
    page_id: &str,
    content: &str,
    allow_intent: bool,
) -> Result<Option<RowsWriteRejection>, String> {
    let Some((skill, _id)) = split_instance_page_id(page_id) else {
        return Ok(None);
    };
    let Some(src) = indexer
        .rows_source(skill)
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    let Ok(parsed) = escurel_md::parse(content) else {
        return Ok(None);
    };
    if !parsed.frontmatter.fields.contains_key("write_back") {
        return Ok(None);
    }
    let reject = |code: &'static str, message: String| {
        Ok(Some(RowsWriteRejection {
            code,
            location: "frontmatter.write_back".to_owned(),
            message,
        }))
    };
    if !allow_intent {
        return reject(
            "write_back_requires_draft",
            "a `write_back` change must be proposed as a draft: a human promotes it, and only then \
             does it reach the source"
                .to_owned(),
        );
    }
    if !Indexer::rows_source_is_writable(&src) {
        return reject(
            "backend_read_only",
            format!(
                "skill `{skill}` reads a `{}` source with no writable columns or no write support; \
                 its rows cannot be written back",
                src.sql.connector.as_str()
            ),
        );
    }
    match crate::write_back::parse_intent(&crate::write_back::frontmatter_json(
        &parsed.frontmatter.fields,
    )) {
        Err(m) => reject("write_back_invalid", m),
        Ok(None) => Ok(None),
        Ok(Some(intent)) => {
            for f in intent.patch.keys() {
                if !src.cfg.writable_columns.contains(f)
                    || Indexer::rows_column_for_field(&src, f).is_none()
                {
                    return Ok(Some(RowsWriteRejection {
                        code: "backend_read_only_field",
                        location: format!("frontmatter.write_back.patch.{f}"),
                        message: format!(
                            "`{f}` is not a writable column of `{skill}` (writable: {:?})",
                            src.cfg.writable_columns
                        ),
                    }));
                }
            }
            Ok(None)
        }
    }
}
