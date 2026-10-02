# A repository could choose where the VS Code extension sends the bearer token

**Symptom.** Found by a security review of the VS Code extension (M4), not by a failing test.
`escurel.gatewayUrl` and the three `escurel.auth.*` settings had no `scope`, which defaults to
`window`, so a cloned repository's `.vscode/settings.json` could set them. After the user clicked
"Trust this workspace" the extension honoured them and attached `Authorization: Bearer <token>` to
the host the repository named. `capabilities.untrustedWorkspaces.restrictedConfigurations` did not
help: it only applies in Restricted Mode, which is the opposite case.

**Fix.** All six security-relevant settings (`gatewayUrl`, `auth.issuer`, `auth.clientId`,
`auth.scopes`, `shellHarness`, `harness`) are `"scope": "application"`, user-level only, and
`test/unit/manifest.test.ts` pins that. A related hole closed in the same change: `activate()`
returned the whole service graph, token store included, to *any* installed extension; it now returns
it only outside `ExtensionMode.Production` (`src/shared/apiExposure.ts`).

**Recognise it next time.**
- A new setting that decides where a credential goes, or what gets executed, must be
  `application` scope. The manifest test lists them; add the new one there.
- Anything that sets such a setting for a test or a demo must write the USER settings
  (`<user-data-dir>/User/settings.json`), never `<workspace>/.vscode/settings.json`: an
  application-scoped setting in a workspace file is ignored, and the symptom is the extension
  talking to the default `http://127.0.0.1:8080` (the integration suite's `before all` hooks
  failed on exactly that).
- `ConfigurationTarget.Workspace` writes to such a setting fail; use `Global`.
