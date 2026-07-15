import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../client/errors.dart';
import '../client/models.dart';
import '../state/providers.dart';
import '../theme/app_theme.dart';

/// The most pasted tarball base64 the import card accepts (~5 MB of
/// text). Larger packs go through the escurel CLI, not a browser paste.
const int packImportMaxTarballChars = 5 * 1024 * 1024;

/// Client-side pre-flight for the paste-based import card: cap the
/// pasted tarball size and require the manifest keys the server needs
/// (`id`, `version`, `content_hash`, `signature`) BEFORE a wire
/// round-trip. Returns a precise error string, or null when sendable.
/// Server-side verification (signature, hash, vertical) still runs —
/// this only turns paste mistakes into instant, named errors.
@visibleForTesting
String? validatePackImportInput(String manifestJson, String tarballB64) {
  if (tarballB64.length > packImportMaxTarballChars) {
    return 'tarball_too_large: pasted base64 is ${tarballB64.length} chars '
        '(max $packImportMaxTarballChars) — use the escurel CLI for large '
        'packs';
  }
  Object? decoded;
  try {
    decoded = jsonDecode(manifestJson);
  } on FormatException catch (e) {
    return 'manifest_invalid_json: ${e.message}';
  }
  if (decoded is! Map<String, dynamic>) {
    return 'manifest_invalid_json: the manifest must be a JSON object';
  }
  final manifest = decoded;
  final missing = [
    'id',
    'version',
    'content_hash',
    'signature',
  ].where((k) => !manifest.containsKey(k)).toList();
  if (missing.isNotEmpty) {
    return 'manifest_missing_keys: ${missing.join(', ')}';
  }
  return null;
}

/// The Backends panel of the Dev Inspector — the operator's trigger surface
/// for the external-instance backends. Four cards, each driving one
/// admin-gated action against the real client:
///
///   1. Credential registry — register / list / delete named source
///      credentials (the secret is write-only; only names + connectors
///      come back).
///   2. Remote endpoints — register / list / delete the named
///      openapi/mcp upstreams remote skills bind to, and probe their
///      reachability (`validate_endpoints`). The secret is write-only.
///   3. Binding health — run `validate_bindings` and read each SQL view's
///      drift status (a degraded binding reads fail-closed).
///   4. Create SQL instance — materialise a read-only view-backed instance
///      from a `sql_view` skill.
///   5. Document ingestion — upload bytes through `/ingest/upload` and watch
///      the evented pipeline's outcome (event id, handler skill, chunk
///      count, or a parked Issue).
///
/// Every interactive widget carries a stable `Semantics(label:)` — the
/// rodney selector contract for `scripts/verify-demo.sh`.
class BackendAdminPanel extends ConsumerWidget {
  const BackendAdminPanel({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    return Semantics(
      label: 'backend-admin-panel',
      identifier: 'backend-admin-panel',
      explicitChildNodes: true,
      child: ListView(
        key: const ValueKey('backend_admin.scroll'),
        padding: const EdgeInsets.all(16),
        children: const [
          _Card(title: 'Source credentials', child: _CredentialRegistry()),
          SizedBox(height: 16),
          _Card(title: 'Remote endpoints', child: _RemoteEndpoints()),
          SizedBox(height: 16),
          _Card(title: 'Binding health', child: _BindingHealth()),
          SizedBox(height: 16),
          _Card(title: 'Create SQL-view instance', child: _CreateSqlInstance()),
          SizedBox(height: 16),
          _Card(title: 'Document ingestion', child: _DocumentIngest()),
          SizedBox(height: 16),
          _Card(title: 'Subscribed packs', child: _SubscribedPacks()),
          SizedBox(height: 16),
          _Card(title: 'Import pack', child: _ImportPack()),
        ],
      ),
    );
  }
}

class _Card extends StatelessWidget {
  const _Card({required this.title, required this.child});

  final String title;
  final Widget child;

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    return Container(
      padding: const EdgeInsets.all(14),
      decoration: BoxDecoration(
        color: kSurfaceContainerLow,
        borderRadius: BorderRadius.circular(10),
        border: Border.all(color: kOutlineVariant),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(title, style: text.titleSmall),
          const SizedBox(height: 10),
          child,
        ],
      ),
    );
  }
}

// ── 1. Credential registry ──────────────────────────────────────────

class _CredentialRegistry extends ConsumerStatefulWidget {
  const _CredentialRegistry();

  @override
  ConsumerState<_CredentialRegistry> createState() =>
      _CredentialRegistryState();
}

class _CredentialRegistryState extends ConsumerState<_CredentialRegistry> {
  final _name = TextEditingController();
  final _secret = TextEditingController();
  String _connector = 'postgres';
  String? _error;
  bool _busy = false;

  static const _connectors = [
    'postgres',
    'mysql',
    'sqlite',
    'erpl',
    'json_dir',
    'parquet_dir',
  ];

  @override
  void dispose() {
    _name.dispose();
    _secret.dispose();
    super.dispose();
  }

  Future<void> _register() async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      await ref
          .read(escurelClientProvider)
          .registerCredential(
            name: _name.text.trim(),
            connector: _connector,
            secret: _secret.text,
          );
      _name.clear();
      _secret.clear();
      ref.invalidate(credentialsProvider);
    } on EscurelClientException catch (e) {
      setState(() => _error = e.message);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _delete(String name) async {
    await ref.read(escurelClientProvider).deleteCredential(name);
    ref.invalidate(credentialsProvider);
  }

  @override
  Widget build(BuildContext context) {
    final creds = ref.watch(credentialsProvider);
    final text = Theme.of(context).textTheme;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Row(
          children: [
            Expanded(
              child: _LabeledField(
                label: 'cred-name-field',
                hint: 'name (e.g. crm_pg)',
                controller: _name,
              ),
            ),
            const SizedBox(width: 8),
            _ConnectorDropdown(
              value: _connector,
              options: _connectors,
              onChanged: (v) => setState(() => _connector = v),
            ),
          ],
        ),
        const SizedBox(height: 8),
        _LabeledField(
          label: 'cred-secret-field',
          hint: 'secret / DSN (write-only)',
          controller: _secret,
          obscure: true,
        ),
        const SizedBox(height: 8),
        Align(
          alignment: Alignment.centerLeft,
          child: _ActionButton(
            label: 'cred-register-button',
            text: _busy ? 'Registering…' : 'Register credential',
            onPressed: _busy ? null : _register,
          ),
        ),
        if (_error != null) _ErrorText(_error!),
        const SizedBox(height: 10),
        creds.when(
          loading: () => const _Spinner(),
          error: (e, _) => _ErrorText('$e'),
          data: (list) => list.isEmpty
              ? Text(
                  'no credentials registered',
                  style: text.bodySmall?.copyWith(color: kOnSurfaceVariant),
                )
              : Semantics(
                  label: 'cred-list',
                  identifier: 'cred-list',
                  explicitChildNodes: true,
                  container: true,
                  child: Column(
                    children: [
                      for (final c in list)
                        Semantics(
                          label: 'cred-item:${c.name}',
                          identifier: 'cred-item:${c.name}',
                          container: true,
                          explicitChildNodes: true,
                          child: ListTile(
                            dense: true,
                            contentPadding: EdgeInsets.zero,
                            title: Text(c.name, style: text.bodyMedium),
                            subtitle: Text(
                              c.connector,
                              style: text.bodySmall?.copyWith(
                                color: kOnSurfaceVariant,
                              ),
                            ),
                            trailing: Semantics(
                              label: 'cred-delete:${c.name}',
                              identifier: 'cred-delete:${c.name}',
                              button: true,
                              child: IconButton(
                                icon: const Icon(
                                  Icons.delete_outline,
                                  size: 18,
                                ),
                                onPressed: () => _delete(c.name),
                              ),
                            ),
                          ),
                        ),
                    ],
                  ),
                ),
        ),
      ],
    );
  }
}

// ── 2. Remote endpoints ─────────────────────────────────────────────

/// The remote-backend endpoint registry (`register_endpoint` /
/// `list_endpoints` / `delete_endpoint`) plus the reachability probe
/// (`validate_endpoints`). openapi/mcp skills bind to these upstreams by
/// NAME — base URL + secret live server-side (the SSRF /
/// secrets-in-markdown guard), so the secret field here is write-only.
/// Stable labels (rodney selector contract): `endpoint-name-field`,
/// `endpoint-kind-field`, `endpoint-url-field`, `endpoint-secret-field`,
/// `endpoint-register-button`, `endpoints-list`, `endpoint-item:<name>`,
/// `endpoint-delete:<name>`, `validate-endpoints-button`.
class _RemoteEndpoints extends ConsumerStatefulWidget {
  const _RemoteEndpoints();

  @override
  ConsumerState<_RemoteEndpoints> createState() => _RemoteEndpointsState();
}

class _RemoteEndpointsState extends ConsumerState<_RemoteEndpoints> {
  final _name = TextEditingController();
  final _url = TextEditingController();
  final _secret = TextEditingController();
  String _kind = 'openapi';
  String? _error;
  bool _busy = false;

  /// Last `validate_endpoints` probe, keyed by endpoint name; rendered on
  /// each row. Null until the operator runs a probe.
  Map<String, EndpointHealth>? _health;

  static const _kinds = ['openapi', 'mcp'];

  @override
  void dispose() {
    _name.dispose();
    _url.dispose();
    _secret.dispose();
    super.dispose();
  }

  Future<void> _register() async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      // A non-empty secret registers bearer auth; empty stays `none`
      // (the server refuses bearer/api_key without a secret).
      final secret = _secret.text;
      await ref
          .read(escurelClientProvider)
          .registerEndpoint(
            name: _name.text.trim(),
            kind: _kind,
            baseUrl: _url.text.trim(),
            auth: secret.isEmpty ? 'none' : 'bearer',
            secret: secret.isEmpty ? null : secret,
          );
      _name.clear();
      _url.clear();
      _secret.clear();
      ref.invalidate(endpointsProvider);
    } on EscurelClientException catch (e) {
      if (mounted) setState(() => _error = e.message);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _delete(String name) async {
    try {
      await ref.read(escurelClientProvider).deleteEndpoint(name);
      if (!mounted) return;
      setState(() => _health?.remove(name));
      ref.invalidate(endpointsProvider);
    } on EscurelClientException catch (e) {
      if (mounted) setState(() => _error = e.message);
    }
  }

  Future<void> _validate() async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      final probes = await ref.read(escurelClientProvider).validateEndpoints();
      if (mounted) {
        setState(() => _health = {for (final h in probes) h.name: h});
      }
    } on EscurelClientException catch (e) {
      if (mounted) setState(() => _error = e.message);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Widget _statusFor(EndpointInfo e, TextTheme text) {
    final probe = _health?[e.name];
    if (probe == null) return const SizedBox.shrink();
    return Padding(
      padding: const EdgeInsets.only(right: 4),
      child: Text(
        probe.status,
        style: text.bodySmall?.copyWith(
          color: probe.healthy ? kSuccess : kError,
        ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final endpoints = ref.watch(endpointsProvider);
    final text = Theme.of(context).textTheme;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Row(
          children: [
            Expanded(
              child: _LabeledField(
                label: 'endpoint-name-field',
                hint: 'name (e.g. yahoo_finance)',
                controller: _name,
              ),
            ),
            const SizedBox(width: 8),
            Semantics(
              label: 'endpoint-kind-field',
              identifier: 'endpoint-kind-field',
              child: DropdownButton<String>(
                value: _kind,
                items: [
                  for (final k in _kinds)
                    DropdownMenuItem(value: k, child: Text(k)),
                ],
                onChanged: (v) => v == null ? null : setState(() => _kind = v),
              ),
            ),
          ],
        ),
        const SizedBox(height: 8),
        _LabeledField(
          label: 'endpoint-url-field',
          hint: 'base URL (REST base or MCP /mcp URL)',
          controller: _url,
        ),
        const SizedBox(height: 8),
        _LabeledField(
          label: 'endpoint-secret-field',
          hint: 'bearer secret (optional, write-only)',
          controller: _secret,
          obscure: true,
        ),
        const SizedBox(height: 8),
        Row(
          children: [
            _ActionButton(
              label: 'endpoint-register-button',
              text: _busy ? 'Registering…' : 'Register endpoint',
              onPressed: _busy ? null : _register,
            ),
            const SizedBox(width: 8),
            _ActionButton(
              label: 'validate-endpoints-button',
              text: _busy ? 'Probing…' : 'Validate endpoints',
              onPressed: _busy ? null : _validate,
            ),
          ],
        ),
        if (_error != null) _ErrorText(_error!),
        const SizedBox(height: 10),
        endpoints.when(
          loading: () => const _Spinner(),
          error: (e, _) => _ErrorText('$e'),
          data: (list) => list.isEmpty
              ? Text(
                  'no endpoints registered',
                  style: text.bodySmall?.copyWith(color: kOnSurfaceVariant),
                )
              : Semantics(
                  label: 'endpoints-list',
                  identifier: 'endpoints-list',
                  explicitChildNodes: true,
                  container: true,
                  child: Column(
                    children: [
                      for (final e in list)
                        Semantics(
                          label: 'endpoint-item:${e.name}',
                          identifier: 'endpoint-item:${e.name}',
                          container: true,
                          explicitChildNodes: true,
                          child: ListTile(
                            dense: true,
                            contentPadding: EdgeInsets.zero,
                            title: Text(e.name, style: text.bodyMedium),
                            subtitle: Text(
                              '${e.kind} · ${e.baseUrl}',
                              style: text.bodySmall?.copyWith(
                                color: kOnSurfaceVariant,
                              ),
                            ),
                            trailing: Row(
                              mainAxisSize: MainAxisSize.min,
                              children: [
                                _statusFor(e, text),
                                Semantics(
                                  label: 'endpoint-delete:${e.name}',
                                  identifier: 'endpoint-delete:${e.name}',
                                  button: true,
                                  child: IconButton(
                                    icon: const Icon(
                                      Icons.delete_outline,
                                      size: 18,
                                    ),
                                    onPressed: () => _delete(e.name),
                                  ),
                                ),
                              ],
                            ),
                          ),
                        ),
                    ],
                  ),
                ),
        ),
      ],
    );
  }
}

// ── 3. Binding health ───────────────────────────────────────────────

class _BindingHealth extends ConsumerStatefulWidget {
  const _BindingHealth();

  @override
  ConsumerState<_BindingHealth> createState() => _BindingHealthState();
}

class _BindingHealthState extends ConsumerState<_BindingHealth> {
  List<BindingStatus>? _result;
  String? _error;
  bool _busy = false;

  Future<void> _validate() async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      final r = await ref.read(escurelClientProvider).validateBindings();
      setState(() => _result = r);
    } on EscurelClientException catch (e) {
      setState(() => _error = e.message);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    final result = _result;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        _ActionButton(
          label: 'validate-bindings-button',
          text: _busy ? 'Validating…' : 'Validate bindings',
          onPressed: _busy ? null : _validate,
        ),
        if (_error != null) _ErrorText(_error!),
        if (result != null) ...[
          const SizedBox(height: 10),
          result.isEmpty
              ? Text(
                  'no SQL-view bindings to check',
                  style: text.bodySmall?.copyWith(color: kOnSurfaceVariant),
                )
              : Semantics(
                  label: 'binding-health-list',
                  identifier: 'binding-health-list',
                  explicitChildNodes: true,
                  container: true,
                  child: Column(
                    children: [
                      for (final b in result)
                        Semantics(
                          label: 'binding-item:${b.view}',
                          identifier: 'binding-item:${b.view}',
                          container: true,
                          explicitChildNodes: true,
                          child: ListTile(
                            dense: true,
                            contentPadding: EdgeInsets.zero,
                            leading: Icon(
                              b.healthy
                                  ? Icons.check_circle_outline
                                  : Icons.error_outline,
                              size: 18,
                              color: b.healthy ? kSuccess : kError,
                            ),
                            title: Text(b.view, style: text.bodyMedium),
                            subtitle: Text(
                              b.detail ?? b.status,
                              style: text.bodySmall?.copyWith(
                                color: kOnSurfaceVariant,
                              ),
                            ),
                          ),
                        ),
                    ],
                  ),
                ),
        ],
      ],
    );
  }
}

// ── 3. Create SQL-view instance ─────────────────────────────────────

class _CreateSqlInstance extends ConsumerStatefulWidget {
  const _CreateSqlInstance();

  @override
  ConsumerState<_CreateSqlInstance> createState() => _CreateSqlInstanceState();
}

class _CreateSqlInstanceState extends ConsumerState<_CreateSqlInstance> {
  final _id = TextEditingController();
  String? _skill;
  String? _resultPageId;
  String? _error;
  bool _busy = false;

  @override
  void dispose() {
    _id.dispose();
    super.dispose();
  }

  Future<void> _create(String skill) async {
    setState(() {
      _busy = true;
      _error = null;
      _resultPageId = null;
    });
    try {
      final pageId = await ref
          .read(escurelClientProvider)
          .createSqlInstance(skill: skill, id: _id.text.trim());
      setState(() => _resultPageId = pageId);
      ref.invalidate(instancesProvider(skill));
    } on EscurelClientException catch (e) {
      setState(() => _error = e.message);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    final catalogue = ref.watch(skillsCatalogueProvider);
    final sqlSkills =
        catalogue.asData?.value
            .where((s) => s.backendKind == 'sql_view')
            .map((s) => s.id)
            .toList() ??
        const <String>[];
    final skill = _skill ?? (sqlSkills.isNotEmpty ? sqlSkills.first : null);

    if (sqlSkills.isEmpty) {
      return Text(
        'no sql_view skills declared',
        style: text.bodySmall?.copyWith(color: kOnSurfaceVariant),
      );
    }
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Row(
          children: [
            Semantics(
              label: 'create-sql-skill-field',
              identifier: 'create-sql-skill-field',
              child: DropdownButton<String>(
                value: skill,
                items: [
                  for (final s in sqlSkills)
                    DropdownMenuItem(value: s, child: Text(s)),
                ],
                onChanged: (v) => setState(() => _skill = v),
              ),
            ),
            const SizedBox(width: 8),
            Expanded(
              child: _LabeledField(
                label: 'create-sql-id-field',
                hint: 'instance id',
                controller: _id,
              ),
            ),
          ],
        ),
        const SizedBox(height: 8),
        _ActionButton(
          label: 'create-sql-submit',
          text: _busy ? 'Creating…' : 'Create instance',
          onPressed: (_busy || skill == null) ? null : () => _create(skill),
        ),
        if (_error != null) _ErrorText(_error!),
        if (_resultPageId != null)
          Semantics(
            label: 'create-sql-result',
            identifier: 'create-sql-result',
            container: true,
            explicitChildNodes: true,
            child: Padding(
              padding: const EdgeInsets.only(top: 8),
              child: Text(
                'created $_resultPageId',
                style: text.bodySmall?.copyWith(color: kSuccess),
              ),
            ),
          ),
      ],
    );
  }
}

// ── 4. Document ingestion ───────────────────────────────────────────

class _DocumentIngest extends ConsumerStatefulWidget {
  const _DocumentIngest();

  @override
  ConsumerState<_DocumentIngest> createState() => _DocumentIngestState();
}

class _DocumentIngestState extends ConsumerState<_DocumentIngest> {
  final _title = TextEditingController();
  final _content = TextEditingController();
  final String _contentType = 'text/plain';
  IngestOutcome? _outcome;
  String? _error;
  bool _busy = false;

  @override
  void dispose() {
    _title.dispose();
    _content.dispose();
    super.dispose();
  }

  Future<void> _ingest() async {
    setState(() {
      _busy = true;
      _error = null;
      _outcome = null;
    });
    try {
      final outcome = await ref
          .read(escurelClientProvider)
          .ingestUpload(
            contentType: _contentType,
            bytes: utf8.encode(_content.text),
            title: _title.text.trim().isEmpty ? null : _title.text.trim(),
          );
      setState(() => _outcome = outcome);
    } on EscurelClientException catch (e) {
      setState(() => _error = e.message);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final outcome = _outcome;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        _LabeledField(
          label: 'ingest-title-field',
          hint: 'title (optional)',
          controller: _title,
        ),
        const SizedBox(height: 8),
        _LabeledField(
          label: 'ingest-text-field',
          hint: 'paste document text',
          controller: _content,
          maxLines: 4,
        ),
        const SizedBox(height: 8),
        _ActionButton(
          label: 'ingest-submit',
          text: _busy ? 'Uploading…' : 'Upload + ingest',
          onPressed: _busy ? null : _ingest,
        ),
        if (_error != null) _ErrorText(_error!),
        if (outcome != null) _IngestOutcomeView(outcome: outcome),
      ],
    );
  }
}

/// The evented pipeline's outcome — the explicit "understand / debug the
/// document processing" surface.
class _IngestOutcomeView extends StatelessWidget {
  const _IngestOutcomeView({required this.outcome});

  final IngestOutcome outcome;

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    final ok = outcome.materialised;
    final rows = <(String, String?)>[
      ('status', outcome.status),
      ('handler', outcome.handlerSkill),
      ('page', outcome.pageId),
      ('event', outcome.eventId),
      ('chunks', outcome.chunkCount?.toString()),
      ('issue', outcome.issueCode),
    ];
    return Semantics(
      label: 'ingest-outcome',
      identifier: 'ingest-outcome',
      container: true,
      explicitChildNodes: true,
      child: Container(
        margin: const EdgeInsets.only(top: 10),
        padding: const EdgeInsets.all(10),
        decoration: BoxDecoration(
          color: (ok ? kSuccess : kError).withValues(alpha: 0.08),
          borderRadius: BorderRadius.circular(8),
          border: Border.all(
            color: (ok ? kSuccess : kError).withValues(alpha: 0.4),
          ),
        ),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            for (final (k, v) in rows)
              if (v != null && v.isNotEmpty)
                Padding(
                  padding: const EdgeInsets.symmetric(vertical: 1),
                  child: Row(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      SizedBox(
                        width: 64,
                        child: Text(
                          k,
                          style: text.labelSmall?.copyWith(
                            color: kOnSurfaceVariant,
                          ),
                        ),
                      ),
                      Expanded(child: Text(v, style: text.bodySmall)),
                    ],
                  ),
                ),
            if (outcome.issueMessage != null)
              Padding(
                padding: const EdgeInsets.only(top: 4),
                child: Text(
                  outcome.issueMessage!,
                  style: text.bodySmall?.copyWith(color: kError),
                ),
              ),
          ],
        ),
      ),
    );
  }
}

// ── shared bits ─────────────────────────────────────────────────────

class _LabeledField extends StatelessWidget {
  const _LabeledField({
    required this.label,
    required this.hint,
    required this.controller,
    this.obscure = false,
    this.maxLines = 1,
  });

  final String label;
  final String hint;
  final TextEditingController controller;
  final bool obscure;
  final int maxLines;

  @override
  Widget build(BuildContext context) {
    return Semantics(
      label: label,
      identifier: label,
      textField: true,
      child: TextField(
        controller: controller,
        obscureText: obscure,
        maxLines: obscure ? 1 : maxLines,
        style: Theme.of(context).textTheme.bodyMedium,
        decoration: InputDecoration(
          isDense: true,
          hintText: hint,
          border: const OutlineInputBorder(),
        ),
      ),
    );
  }
}

class _ConnectorDropdown extends StatelessWidget {
  const _ConnectorDropdown({
    required this.value,
    required this.options,
    required this.onChanged,
  });

  final String value;
  final List<String> options;
  final ValueChanged<String> onChanged;

  @override
  Widget build(BuildContext context) {
    return Semantics(
      label: 'cred-connector-field',
      identifier: 'cred-connector-field',
      child: DropdownButton<String>(
        value: value,
        items: [
          for (final o in options) DropdownMenuItem(value: o, child: Text(o)),
        ],
        onChanged: (v) => v == null ? null : onChanged(v),
      ),
    );
  }
}

class _ActionButton extends StatelessWidget {
  const _ActionButton({
    required this.label,
    required this.text,
    required this.onPressed,
  });

  final String label;
  final String text;
  final VoidCallback? onPressed;

  @override
  Widget build(BuildContext context) {
    return Semantics(
      label: label,
      identifier: label,
      button: true,
      onTap: onPressed,
      excludeSemantics: true,
      child: FilledButton(onPressed: onPressed, child: Text(text)),
    );
  }
}

class _ErrorText extends StatelessWidget {
  const _ErrorText(this.message);

  final String message;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.only(top: 6),
      child: Text(
        message,
        style: Theme.of(context).textTheme.bodySmall?.copyWith(color: kError),
      ),
    );
  }
}

class _Spinner extends StatelessWidget {
  const _Spinner();

  @override
  Widget build(BuildContext context) => const Padding(
    padding: EdgeInsets.all(8),
    child: SizedBox(
      height: 14,
      width: 14,
      child: CircularProgressIndicator(strokeWidth: 1.5),
    ),
  );
}

/// Card 5 — the subscribed skill packs and their pinned versions
/// (`list_packs`, REQ-SUB-01): the provenance behind every read-only
/// `base@<pack>@<version>` page. Refresh-on-demand like the binding
/// health card; carries stable `packs-list` / `pack-item:<id>`
/// semantics labels (the rodney selector contract).
class _SubscribedPacks extends ConsumerStatefulWidget {
  const _SubscribedPacks();

  @override
  ConsumerState<_SubscribedPacks> createState() => _SubscribedPacksState();
}

class _SubscribedPacksState extends ConsumerState<_SubscribedPacks> {
  List<PackSubscriptionInfo>? _result;
  String? _error;
  bool _busy = false;

  /// Pack id armed for unsubscribe — the confirm step. Tapping the
  /// unsubscribe icon only arms; the destructive call fires on the
  /// explicit confirm tap.
  String? _armedUnsubscribe;

  Future<void> _refresh() async {
    setState(() {
      _busy = true;
      _error = null;
    });
    // Every setState after the await checks `mounted` — the user can
    // navigate away mid-request (agy review; the older cards predate
    // this discipline).
    try {
      final r = await ref.read(escurelClientProvider).listPacks();
      if (mounted) setState(() => _result = r);
    } on EscurelClientException catch (e) {
      if (mounted) setState(() => _error = e.message);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _unsubscribe(String packId) async {
    // The armed state is deliberately KEPT while the call is in flight —
    // clearing it up front flips the row back to the unarmed icon
    // mid-request (review finding). The confirm button disables via
    // `_busy` instead; the armed state clears on completion.
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      await ref.read(escurelClientProvider).unsubscribePack(packId);
      if (!mounted) return;
      // Re-read the pack list so the row disappears from the source of
      // truth, not from local bookkeeping.
      final r = await ref.read(escurelClientProvider).listPacks();
      if (mounted) setState(() => _result = r);
    } on EscurelClientException catch (e) {
      if (mounted) setState(() => _error = e.message);
    } finally {
      if (mounted) {
        setState(() {
          _busy = false;
          _armedUnsubscribe = null;
        });
      }
    }
  }

  Widget _trailingFor(PackSubscriptionInfo p) {
    if (_armedUnsubscribe == p.packId) {
      return Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Semantics(
            label: 'pack-unsubscribe-confirm:${p.packId}',
            identifier: 'pack-unsubscribe-confirm:${p.packId}',
            button: true,
            onTap: _busy ? null : () => _unsubscribe(p.packId),
            excludeSemantics: true,
            child: FilledButton(
              style: FilledButton.styleFrom(
                backgroundColor: kError,
                visualDensity: VisualDensity.compact,
              ),
              onPressed: _busy ? null : () => _unsubscribe(p.packId),
              child: Text(_busy ? 'Removing…' : 'Confirm'),
            ),
          ),
          IconButton(
            icon: const Icon(Icons.close, size: 16),
            tooltip: 'cancel',
            onPressed: _busy
                ? null
                : () => setState(() => _armedUnsubscribe = null),
          ),
        ],
      );
    }
    return Semantics(
      label: 'pack-unsubscribe:${p.packId}',
      identifier: 'pack-unsubscribe:${p.packId}',
      button: true,
      onTap: _busy ? null : () => setState(() => _armedUnsubscribe = p.packId),
      excludeSemantics: true,
      child: IconButton(
        icon: const Icon(Icons.link_off, size: 18),
        tooltip: 'unsubscribe',
        onPressed: _busy
            ? null
            : () => setState(() => _armedUnsubscribe = p.packId),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    final result = _result;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        _ActionButton(
          label: 'list-packs-button',
          text: _busy ? 'Loading…' : 'List packs',
          onPressed: _busy ? null : _refresh,
        ),
        if (_error != null) _ErrorText(_error!),
        if (result != null) ...[
          const SizedBox(height: 10),
          result.isEmpty
              ? Text(
                  'no packs subscribed — this node runs on its own overlay only',
                  style: text.bodySmall?.copyWith(color: kOnSurfaceVariant),
                )
              : Semantics(
                  label: 'packs-list',
                  identifier: 'packs-list',
                  explicitChildNodes: true,
                  container: true,
                  child: Column(
                    children: [
                      for (final p in result)
                        Semantics(
                          label: 'pack-item:${p.packId}',
                          identifier: 'pack-item:${p.packId}',
                          container: true,
                          explicitChildNodes: true,
                          child: ListTile(
                            dense: true,
                            leading: const Icon(
                              Icons.inventory_2_outlined,
                              size: 18,
                            ),
                            title: Text('${p.packId}@v${p.version}'),
                            subtitle: Text(
                              'vertical ${p.vertical} · ${p.publisher}',
                              style: text.bodySmall,
                            ),
                            trailing: _trailingFor(p),
                          ),
                        ),
                    ],
                  ),
                ),
        ],
      ],
    );
  }
}

/// Card 7 — import a signed skill pack as this node's read-only base
/// layer (`import_pack`, REQ-SUB-01..03). Paste-based on purpose: the
/// web-lite explorer takes the manifest JSON + tarball base64 as text
/// (file pickers are out of scope). Server refusal codes
/// (`pack_signature_invalid`, `vertical_mismatch`, …) surface verbatim.
/// Stable labels: `pack-import-manifest-field`, `pack-import-tarball-field`,
/// `pack-import-allow-mismatch`, `pack-import-submit`, `pack-import-result`.
class _ImportPack extends ConsumerStatefulWidget {
  const _ImportPack();

  @override
  ConsumerState<_ImportPack> createState() => _ImportPackState();
}

class _ImportPackState extends ConsumerState<_ImportPack> {
  final _manifest = TextEditingController();
  final _tarball = TextEditingController();
  bool _allowVerticalMismatch = false;
  PackOpResult? _outcome;
  String? _error;
  bool _busy = false;

  @override
  void dispose() {
    _manifest.dispose();
    _tarball.dispose();
    super.dispose();
  }

  Future<void> _import() async {
    // Pre-flight the paste before any wire round-trip: size cap +
    // required manifest keys, with a precise named error.
    final preflight = validatePackImportInput(
      _manifest.text.trim(),
      _tarball.text.trim(),
    );
    if (preflight != null) {
      setState(() {
        _error = preflight;
        _outcome = null;
      });
      return;
    }
    setState(() {
      _busy = true;
      _error = null;
      _outcome = null;
    });
    try {
      final r = await ref
          .read(escurelClientProvider)
          .importPack(
            _manifest.text.trim(),
            _tarball.text.trim(),
            allowVerticalMismatch: _allowVerticalMismatch,
          );
      if (mounted) setState(() => _outcome = r);
    } on EscurelClientException catch (e) {
      // The server's refusal codes are the operator's diagnosis —
      // show the message verbatim, never paraphrased.
      if (mounted) setState(() => _error = e.message);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    final outcome = _outcome;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        _LabeledField(
          label: 'pack-import-manifest-field',
          hint: 'manifest JSON (id / version / vertical / signature …)',
          controller: _manifest,
          maxLines: 3,
        ),
        const SizedBox(height: 8),
        _LabeledField(
          label: 'pack-import-tarball-field',
          hint: 'pack tarball, base64',
          controller: _tarball,
          maxLines: 3,
        ),
        const SizedBox(height: 8),
        Semantics(
          label: 'pack-import-allow-mismatch',
          identifier: 'pack-import-allow-mismatch',
          container: true,
          explicitChildNodes: true,
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              Checkbox(
                value: _allowVerticalMismatch,
                visualDensity: VisualDensity.compact,
                onChanged: (v) =>
                    setState(() => _allowVerticalMismatch = v ?? false),
              ),
              Text(
                'allow vertical mismatch (REQ-SUB-03 override)',
                style: text.bodySmall,
              ),
            ],
          ),
        ),
        const SizedBox(height: 8),
        _ActionButton(
          label: 'pack-import-submit',
          text: _busy ? 'Importing…' : 'Import pack',
          onPressed: _busy ? null : _import,
        ),
        if (_error != null) _ErrorText(_error!),
        if (outcome != null)
          Semantics(
            label: 'pack-import-result',
            identifier: 'pack-import-result',
            container: true,
            explicitChildNodes: true,
            child: Padding(
              padding: const EdgeInsets.only(top: 8),
              child: Text(
                'imported ${outcome.pack}@v${outcome.version} '
                '(${outcome.pagesImported ?? 0} pages)',
                style: text.bodySmall?.copyWith(color: kSuccess),
              ),
            ),
          ),
      ],
    );
  }
}
