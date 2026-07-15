import 'package:flutter/material.dart';

import '../client/models.dart';
import '../theme/app_theme.dart';

/// Renders the *external-backend* section of an instance page beneath the
/// overlay frontmatter: a bounded SQL-view projection, a document's
/// chunk/extraction status, or a remote (openapi/mcp) instance's LIVE
/// upstream projection. Native `markdown` instances render nothing.
///
/// This is the "understand & debug from a high level" surface (the explicit
/// ask): an operator sees which backend a page is bound to, the live view
/// rows or the chunk count, and any fail-closed Issue (`binding_degraded`)
/// without leaving the page. Read-only is stated plainly — these backends
/// reject `update_page` server-side.
///
/// Stable semantics labels (`backend-pane:<id>`, `backend-projection`,
/// `document-chunks`, `binding-degraded`) are the rodney selector contract.
class BackendPane extends StatelessWidget {
  const BackendPane({super.key, required this.page});

  final ExpandResult page;

  @override
  Widget build(BuildContext context) {
    final kind = page.backendKind;
    if (kind == null || kind == 'markdown') return const SizedBox.shrink();

    return Semantics(
      label: 'backend-pane:${page.pageId}',
      identifier: 'backend-pane:${page.pageId}',
      explicitChildNodes: true,
      child: Container(
        key: const ValueKey('entity_editor.backend'),
        margin: const EdgeInsets.only(top: 16),
        padding: const EdgeInsets.all(12),
        decoration: BoxDecoration(
          color: kSurfaceContainerLow,
          borderRadius: BorderRadius.circular(8),
          border: Border.all(color: kOutlineVariant),
        ),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            _Header(kind: kind, backendRef: page.backendRef),
            const SizedBox(height: 10),
            if (kind == 'sql_view')
              _SqlProjection(projection: page.backendProjection)
            else if (kind == 'document')
              _DocumentStatus(page: page)
            else if (kind == 'openapi' || kind == 'mcp')
              _RemoteProjection(projection: page.backendProjection),
          ],
        ),
      ),
    );
  }
}

class _Header extends StatelessWidget {
  const _Header({required this.kind, required this.backendRef});

  final String kind;
  final Map<String, dynamic>? backendRef;

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    final label = switch (kind) {
      'sql_view' => 'SQL view (read-only)',
      'document' => 'Document (read-only)',
      // Remote instances hold no local data — every expand is a live
      // upstream read against the named endpoint.
      _ => 'Remote $kind (live)',
    };
    final ref =
        backendRef?['view'] ??
        backendRef?['blob_id'] ??
        backendRef?['endpoint'] ??
        '';
    return Row(
      children: [
        const Icon(Icons.lock_outline, size: 14, color: kOnSurfaceVariant),
        const SizedBox(width: 6),
        Text(label, style: text.labelLarge?.copyWith(color: kOnSurfaceVariant)),
        const SizedBox(width: 8),
        Expanded(
          child: Text(
            '$ref',
            style: text.bodySmall?.copyWith(color: kOnSurfaceVariant),
            overflow: TextOverflow.ellipsis,
          ),
        ),
      ],
    );
  }
}

class _SqlProjection extends StatelessWidget {
  const _SqlProjection({required this.projection});

  final BackendProjection? projection;

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    final p = projection;
    if (p == null) {
      return Text(
        'no projection returned',
        style: text.bodySmall?.copyWith(color: kOnSurfaceVariant),
      );
    }
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        if (p.degraded)
          _DegradedBanner(code: p.issueCode, message: p.issueMessage),
        if (p.degraded) const SizedBox(height: 8),
        Semantics(
          label: 'backend-projection',
          identifier: 'backend-projection',
          container: true,
          explicitChildNodes: true,
          child: _RowsTable(rows: p.rows),
        ),
        if (p.truncated)
          Padding(
            padding: const EdgeInsets.only(top: 6),
            child: Text(
              'bounded projection — more rows in the source view',
              style: text.bodySmall?.copyWith(color: kOnSurfaceVariant),
            ),
          ),
      ],
    );
  }
}

class _RowsTable extends StatelessWidget {
  const _RowsTable({required this.rows});

  final List<Map<String, dynamic>> rows;

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    if (rows.isEmpty) {
      return Text(
        'view returned no rows',
        style: text.bodySmall?.copyWith(color: kOnSurfaceVariant),
      );
    }
    // Union the keys across rows so a sparse first row doesn't hide columns.
    final columns = <String>[];
    for (final r in rows) {
      for (final k in r.keys) {
        if (!columns.contains(k)) columns.add(k);
      }
    }
    return SingleChildScrollView(
      scrollDirection: Axis.horizontal,
      child: DataTable(
        headingRowHeight: 28,
        dataRowMinHeight: 24,
        dataRowMaxHeight: 36,
        columnSpacing: 20,
        columns: [
          for (final c in columns)
            DataColumn(
              label: Text(
                c,
                style: text.labelMedium?.copyWith(color: kOnSurfaceVariant),
              ),
            ),
        ],
        rows: [
          for (final r in rows)
            DataRow(
              cells: [
                for (final c in columns)
                  DataCell(Text('${r[c] ?? '—'}', style: text.bodySmall)),
              ],
            ),
        ],
      ),
    );
  }
}

/// The live upstream projection of a remote (`openapi`/`mcp`) instance:
/// the projected field→value rows plus the endpoint they were fetched
/// from, or — the fail-closed path — the plain-string issue rendered
/// prominently. Nothing is materialised locally; every expand re-reads
/// the upstream.
class _RemoteProjection extends StatelessWidget {
  const _RemoteProjection({required this.projection});

  final BackendProjection? projection;

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    final p = projection;
    if (p == null) {
      return Text(
        'no live projection returned',
        style: text.bodySmall?.copyWith(color: kOnSurfaceVariant),
      );
    }
    if (p.degraded) {
      // Fail closed: the overlay page still renders, but no fields table
      // pretends data came back — only the upstream's issue, verbatim.
      return _DegradedBanner(code: p.issueCode, message: p.issueMessage);
    }
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Semantics(
          label: 'backend-projection',
          identifier: 'backend-projection',
          container: true,
          explicitChildNodes: true,
          child: p.fields.isEmpty
              ? Text(
                  'upstream returned no projected fields',
                  style: text.bodySmall?.copyWith(color: kOnSurfaceVariant),
                )
              : Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    for (final e in p.fields.entries)
                      Padding(
                        padding: const EdgeInsets.symmetric(vertical: 1),
                        child: Row(
                          crossAxisAlignment: CrossAxisAlignment.start,
                          children: [
                            SizedBox(
                              width: 96,
                              child: Text(
                                e.key,
                                style: text.labelSmall?.copyWith(
                                  color: kOnSurfaceVariant,
                                ),
                              ),
                            ),
                            Expanded(
                              child: Text(
                                '${e.value ?? '—'}',
                                style: text.bodySmall,
                              ),
                            ),
                          ],
                        ),
                      ),
                  ],
                ),
        ),
        if (p.endpoint != null)
          Padding(
            padding: const EdgeInsets.only(top: 6),
            child: Text(
              'live from ${p.endpoint} — fetched on expand, nothing stored',
              style: text.bodySmall?.copyWith(color: kOnSurfaceVariant),
            ),
          ),
      ],
    );
  }
}

class _DocumentStatus extends StatelessWidget {
  const _DocumentStatus({required this.page});

  final ExpandResult page;

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    final ref = page.backendRef ?? const {};
    final status = (ref['status'] as String?) ?? 'materialised';
    final engine = ref['extract_engine'] as String?;
    final total = page.chunksTotal ?? page.blocks.length;
    final shown = page.blocks.length;
    return Semantics(
      label: 'document-chunks',
      identifier: 'document-chunks',
      container: true,
      explicitChildNodes: true,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Wrap(
            spacing: 8,
            runSpacing: 4,
            crossAxisAlignment: WrapCrossAlignment.center,
            children: [
              _StatusChip(status: status),
              if (engine != null)
                Text(
                  'via $engine',
                  style: text.bodySmall?.copyWith(color: kOnSurfaceVariant),
                ),
              Text(
                page.chunksTruncated
                    ? 'showing $shown of $total chunks'
                    : '$total chunks',
                style: text.bodySmall?.copyWith(color: kOnSurfaceVariant),
              ),
            ],
          ),
          if (page.chunksTruncated)
            Padding(
              padding: const EdgeInsets.only(top: 6),
              child: Text(
                'lead chunks only — full text lives in the source blob',
                style: text.bodySmall?.copyWith(color: kOnSurfaceVariant),
              ),
            ),
        ],
      ),
    );
  }
}

class _StatusChip extends StatelessWidget {
  const _StatusChip({required this.status});

  final String status;

  @override
  Widget build(BuildContext context) {
    // The server stamps a healthy document's backend_ref `status: ok`; the
    // ingest pipeline's outcome calls the same success `materialised`. Treat
    // both as green so a freshly-ingested page doesn't read as failed.
    final ok = status == 'ok' || status == 'materialised';
    final (bg, fg) = ok
        ? (kSecondaryContainer, kOnSecondaryContainer)
        : (kError, kSurfaceContainerLowest);
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
      decoration: BoxDecoration(
        color: bg,
        borderRadius: BorderRadius.circular(6),
      ),
      child: Text(
        status,
        style: Theme.of(
          context,
        ).textTheme.labelSmall?.copyWith(color: fg, fontSize: 9),
      ),
    );
  }
}

class _DegradedBanner extends StatelessWidget {
  const _DegradedBanner({this.code, this.message});

  /// sql_view issues carry a code; remote failures are a bare message.
  final String? code;
  final String? message;

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    return Semantics(
      label: 'binding-degraded',
      identifier: 'binding-degraded',
      container: true,
      explicitChildNodes: true,
      child: Container(
        padding: const EdgeInsets.all(8),
        decoration: BoxDecoration(
          color: kError.withValues(alpha: 0.08),
          borderRadius: BorderRadius.circular(6),
          border: Border.all(color: kError.withValues(alpha: 0.4)),
        ),
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            const Icon(Icons.warning_amber_rounded, size: 16, color: kError),
            const SizedBox(width: 8),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  if (code != null)
                    Text(
                      code!,
                      style: text.labelMedium?.copyWith(color: kError),
                    ),
                  if (message != null)
                    Text(
                      message!,
                      // With no code (a remote string issue) the message IS
                      // the headline — render it in the error style.
                      style: code == null
                          ? text.labelMedium?.copyWith(color: kError)
                          : text.bodySmall?.copyWith(color: kOnSurface),
                    ),
                  Text(
                    'reads fail closed until the binding is revalidated',
                    style: text.bodySmall?.copyWith(color: kOnSurfaceVariant),
                  ),
                ],
              ),
            ),
          ],
        ),
      ),
    );
  }
}
