import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../client/errors.dart';
import '../client/models.dart';
import '../state/providers.dart';
import '../theme/app_theme.dart';

/// The Backends panel of the Dev Inspector — the operator's trigger surface
/// for the external-instance backends. Four cards, each driving one
/// admin-gated action against the real client:
///
///   1. Credential registry — register / list / delete named source
///      credentials (the secret is write-only; only names + connectors
///      come back).
///   2. Binding health — run `validate_bindings` and read each SQL view's
///      drift status (a degraded binding reads fail-closed).
///   3. Create SQL instance — materialise a read-only view-backed instance
///      from a `sql_view` skill.
///   4. Document ingestion — upload bytes through `/ingest/upload` and watch
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
          _Card(title: 'Binding health', child: _BindingHealth()),
          SizedBox(height: 16),
          _Card(title: 'Create SQL-view instance', child: _CreateSqlInstance()),
          SizedBox(height: 16),
          _Card(title: 'Document ingestion', child: _DocumentIngest()),
          SizedBox(height: 16),
          _Card(title: 'Subscribed packs', child: _SubscribedPacks()),
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

// ── 2. Binding health ───────────────────────────────────────────────

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

  Future<void> _refresh() async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      final r = await ref.read(escurelClientProvider).listPacks();
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
                            leading: const Icon(Icons.inventory_2_outlined, size: 18),
                            title: Text('${p.packId}@v${p.version}'),
                            subtitle: Text(
                              'vertical ${p.vertical} · ${p.publisher}',
                              style: text.bodySmall,
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
