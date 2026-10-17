# duckvfs on SharePoint: what the extension accepts and what escurel must check itself

Found while wiring a `sharepoint://` root into the `duckvfs` lane store (extension v1.5.6 from the erpl.io mirror).

- **The extension does not validate the target.** `CREATE SECRET (TYPE sharepoint, PROVIDER client_credentials, TENANT_ID,
  CLIENT_ID, CLIENT_SECRET)` succeeds with neither `DRIVE_ID` nor `SITE_ID`; the failure would come on the first read.
  Escurel therefore requires exactly one of the two before DuckDB is touched (`SharepointCredentials::validate`), the
  mirror of the gdrive My-Drive guard.
- **`GRAPH_SCOPE`, never `SCOPE`.** DuckDB's `CREATE SECRET` reserves `SCOPE` for the secret's path-matching prefix; an
  OAuth scope under that name silently rescopes the secret to match nothing. Same trap as gdrive's `DRIVE_SCOPE`. The
  statement builder is pinned by a test.
- **Unknown options are a Binder Error** ("Unknown parameter 'x' for secret type 'sharepoint'"), which does not echo the
  statement. The client secret is still scrubbed from any error text before it leaves the store, because DuckDB does echo
  statements for some failures.
- **Quotes stay inside their literal.** `CREATE SECRET` cannot be parameterised, so values are spliced with `''`
  escaping; a test shows a `'` in `GRAPH_SCOPE` does not inject an option.
- **Forbidden names are refused up front** (`" * : < > ? \ |`), naming the character, because a failure deep in DuckDB
  reads as an IO fault rather than a content problem.
- **Recognise it:** a `sharepoint://` store that opens cleanly and fails on the first read with a Graph "not found" for the
  root means the target was missing or wrong, not that the file is absent.
