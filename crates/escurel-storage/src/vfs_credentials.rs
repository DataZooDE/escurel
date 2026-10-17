//! Credentials for a `sharepoint://` lane store.
//!
//! Always compiled (no `duckvfs` feature needed) so the server's config layer
//! can hold and validate them, while only the `duckvfs` store ever spends
//! them. The one rule here: **the client secret never leaves this type
//! except through [`SecretString::expose`]**, which exactly one call site
//! (the `CREATE SECRET` statement) uses. `Debug` and `Display` print
//! `<redacted>`, and [`SecretString::scrub`] removes the value from any text
//! (a DuckDB error echoes the statement it failed on) before it is logged or
//! returned.

use std::fmt;

/// A value that must not reach a log, an error message or a config dump.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The raw value. Call it only where the secret is spent.
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// `text` with every occurrence of the secret replaced by `<redacted>`.
    pub fn scrub(&self, text: &str) -> String {
        if self.0.is_empty() {
            return text.to_owned();
        }
        text.replace(&self.0, "<redacted>")
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretString(<redacted>)")
    }
}

impl fmt::Display for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// What the `sharepoint` DuckDB extension's `client_credentials` provider
/// needs: an Entra app registration and the document library it may reach.
#[derive(Debug, Clone, Default)]
pub struct SharepointCredentials {
    /// Entra directory (tenant) id or domain.
    pub tenant_id: String,
    /// Entra application (client) id.
    pub client_id: String,
    pub client_secret: SecretString,
    /// Graph drive id (`b!…`) of the document library the paths hang off.
    pub drive_id: Option<String>,
    /// Graph site id; resolved to the site's default library. Exactly one of
    /// `drive_id` / `site_id` is set.
    pub site_id: Option<String>,
    /// OAuth scope override (`GRAPH_SCOPE`, never `SCOPE`); the extension
    /// defaults to `<graph endpoint>/.default`.
    pub graph_scope: Option<String>,
}

/// Why a [`SharepointCredentials`] was refused. Messages name the field,
/// never its value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CredentialError {
    #[error("sharepoint credentials: {0} is empty")]
    Empty(&'static str),
    #[error("sharepoint credentials: set exactly one of drive_id and site_id")]
    Target,
    #[error("sharepoint credentials: {0} contains a control character")]
    ControlCharacter(&'static str),
}

impl SharepointCredentials {
    /// Refuse an incomplete or ambiguous credential set.
    ///
    /// A target is required: without a drive or site id the extension has no
    /// library to root the paths in, and the failure would surface only on the
    /// first read.
    pub fn validate(&self) -> Result<(), CredentialError> {
        let field = |name: &'static str, value: &str| {
            if value.trim().is_empty() {
                Err(CredentialError::Empty(name))
            } else if value.chars().any(char::is_control) {
                Err(CredentialError::ControlCharacter(name))
            } else {
                Ok(())
            }
        };
        field("tenant_id", &self.tenant_id)?;
        field("client_id", &self.client_id)?;
        field("client_secret", self.client_secret.expose())?;
        match (&self.drive_id, &self.site_id) {
            (Some(id), None) => field("drive_id", id)?,
            (None, Some(id)) => field("site_id", id)?,
            _ => return Err(CredentialError::Target),
        }
        if let Some(scope) = &self.graph_scope {
            field("graph_scope", scope)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok() -> SharepointCredentials {
        SharepointCredentials {
            tenant_id: "00000000-0000-0000-0000-000000000000".to_owned(),
            client_id: "11111111-1111-1111-1111-111111111111".to_owned(),
            client_secret: SecretString::new("s3cr3t-value"),
            drive_id: Some("b!AbCdEf".to_owned()),
            site_id: None,
            graph_scope: None,
        }
    }

    #[test]
    fn a_complete_credential_set_validates() {
        ok().validate().expect("complete set");
        let by_site = SharepointCredentials {
            drive_id: None,
            site_id: Some("contoso.sharepoint.com,site,web".to_owned()),
            ..ok()
        };
        by_site.validate().expect("site id is a valid target");
    }

    #[test]
    fn every_required_field_is_enforced() {
        let blank_tenant = SharepointCredentials {
            tenant_id: String::new(),
            ..ok()
        };
        assert_eq!(
            blank_tenant.validate(),
            Err(CredentialError::Empty("tenant_id"))
        );
        let blank_client = SharepointCredentials {
            client_id: " ".to_owned(),
            ..ok()
        };
        assert_eq!(
            blank_client.validate(),
            Err(CredentialError::Empty("client_id"))
        );
        let blank_secret = SharepointCredentials {
            client_secret: SecretString::new(""),
            ..ok()
        };
        assert_eq!(
            blank_secret.validate(),
            Err(CredentialError::Empty("client_secret"))
        );
    }

    #[test]
    fn exactly_one_target_is_required() {
        let none = SharepointCredentials {
            drive_id: None,
            ..ok()
        };
        assert_eq!(none.validate(), Err(CredentialError::Target));
        let both = SharepointCredentials {
            site_id: Some("contoso.sharepoint.com,site,web".to_owned()),
            ..ok()
        };
        assert_eq!(both.validate(), Err(CredentialError::Target));
    }

    #[test]
    fn a_control_character_cannot_be_spliced_into_the_statement() {
        let c = SharepointCredentials {
            client_id: "abc\ndef".to_owned(),
            ..ok()
        };
        assert_eq!(
            c.validate(),
            Err(CredentialError::ControlCharacter("client_id"))
        );
        let s = SharepointCredentials {
            client_secret: SecretString::new("pass\0word"),
            ..ok()
        };
        assert_eq!(
            s.validate(),
            Err(CredentialError::ControlCharacter("client_secret"))
        );
    }

    #[test]
    fn the_secret_never_prints() {
        let c = ok();
        for rendered in [
            format!("{c:?}"),
            format!("{:?}", c.client_secret),
            format!("{}", c.client_secret),
            format!("{:#?}", Some(c.clone())),
        ] {
            assert!(!rendered.contains("s3cr3t-value"), "{rendered}");
        }
        assert!(format!("{c:?}").contains("<redacted>"));
        // The refusal messages name fields only.
        let err = SharepointCredentials {
            tenant_id: String::new(),
            ..c
        }
        .validate()
        .unwrap_err();
        assert!(!err.to_string().contains("s3cr3t-value"));
    }

    #[test]
    fn scrub_removes_the_secret_from_any_text() {
        let s = SecretString::new("s3cr3t-value");
        let text = "Parser Error near CLIENT_SECRET 's3cr3t-value', x; again s3cr3t-value";
        let out = s.scrub(text);
        assert!(!out.contains("s3cr3t-value"), "{out}");
        assert_eq!(out.matches("<redacted>").count(), 2);
        // An empty secret must not turn every position into a match.
        assert_eq!(SecretString::new("").scrub("abc"), "abc");
    }
}
