/// Dev inspector panel: the registered runners (`list_runtimes`, #818) —
/// whether each is online, stale or stopped (in words and by icon shape,
/// never colour alone), the harnesses it offers with their CLI versions
/// (an admin's read; a member's carries names only), its labels and what
/// it is running. The directory is readable by any token.
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../client/errors.dart';
import '../client/models.dart';
import '../state/providers.dart';
import '../theme/app_theme.dart';

final runtimesProvider = FutureProvider<List<RuntimeInfo>>(
  (ref) async =>
      (await ref.watch(escurelClientProvider).listRuntimes()).toList()
        ..sort((a, b) => a.runnerId.compareTo(b.runnerId)),
);

class RuntimesPanel extends ConsumerWidget {
  const RuntimesPanel({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final runtimes = ref.watch(runtimesProvider);
    return Semantics(
      label: 'runtimes-panel',
      container: true,
      explicitChildNodes: true,
      child: runtimes.when(
        loading: () =>
            const Center(child: CircularProgressIndicator(strokeWidth: 2)),
        error: (e, _) => Padding(
          padding: const EdgeInsets.all(16),
          child: Text(
            'error: ${humanizeEscurelError(e)}',
            style: Theme.of(context).textTheme.bodySmall
                ?.copyWith(color: kError),
          ),
        ),
        data: (list) {
          final text = Theme.of(context).textTheme;
          final online = list.where((r) => r.status == 'online').length;
          return ListView(
            padding: const EdgeInsets.all(12),
            children: [
              Padding(
                padding: const EdgeInsets.symmetric(vertical: 6),
                child: Text(
                  'RUNTIMES · ${list.length} · $online online',
                  style: text.labelSmall?.copyWith(
                    color: kOutline,
                    letterSpacing: 1,
                  ),
                ),
              ),
              if (list.isEmpty)
                Text(
                  'no runtimes yet — a runner registers here when it starts',
                  style: text.bodySmall?.copyWith(color: kOnSurfaceVariant),
                ),
              for (final r in list) _RuntimeRow(runtime: r),
            ],
          );
        },
      ),
    );
  }
}

/// Icon shape and words per status; `unknown` covers any newer value too.
(IconData, String, Color) _statusLook(String status) => switch (status) {
  'online' => (Icons.check_circle_outline, 'online', kSuccess),
  'stale' => (Icons.warning_amber_outlined, 'stale', kWarning),
  'stopped' => (Icons.stop_circle_outlined, 'stopped', kOnSurfaceVariant),
  _ => (Icons.help_outline, 'not reporting', kOnSurfaceVariant),
};

String _seconds(int s) => s < 120 ? '$s s' : '${(s / 60).round()} min';

String _seen(RuntimeInfo r) {
  final age = r.ageS;
  if (age == null) return 'no status record yet';
  final seen = age < 60 ? 'just now' : '${_seconds(age)} ago';
  final after = r.staleAfterS == null
      ? ''
      : ' · stale after ${_seconds(r.staleAfterS!)}';
  return 'last seen $seen$after';
}

class _RuntimeRow extends StatelessWidget {
  const _RuntimeRow({required this.runtime});

  final RuntimeInfo runtime;

  @override
  Widget build(BuildContext context) {
    final r = runtime;
    final text = Theme.of(context).textTheme;
    final muted = text.labelSmall?.copyWith(color: kOnSurfaceVariant);
    final (icon, word, colour) = _statusLook(r.status);
    final harnesses = r.harnesses.isEmpty
        ? 'no harness'
        // In words, never colour alone: a harness whose CLI probe failed
        // runs nothing, and work that requires it goes elsewhere.
        : r.harnesses
              .map(
                (h) => [
                  h.name,
                  ?h.version,
                  if (!h.available) '(unavailable)',
                ].join(' '),
              )
              .join(', ');
    final down = r.harnesses.where((h) => !h.available).toList();
    final busy = r.liveRuns == 0
        ? 'idle'
        : '${r.liveRuns} live ${r.liveRuns == 1 ? 'run' : 'runs'}';
    return Semantics(
      label: 'runtime-row:${r.runnerId}',
      container: true,
      explicitChildNodes: true,
      child: Container(
        margin: const EdgeInsets.symmetric(vertical: 3),
        padding: const EdgeInsets.all(8),
        decoration: BoxDecoration(
          color: kSurfaceContainerLow,
          borderRadius: BorderRadius.circular(6),
          border: Border.all(color: kOutlineVariant),
        ),
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(
                    r.runnerId,
                    style: text.bodySmall?.copyWith(
                      fontWeight: FontWeight.w600,
                    ),
                  ),
                  Text(harnesses, style: muted),
                  for (final h in down)
                    Text(
                      '${h.name} unavailable: its CLI probe failed'
                      '${h.probeError == null ? '' : ' (${h.probeError})'}',
                      style: muted,
                    ),
                  Text(
                    'labels: ${r.labels.isEmpty ? 'none' : r.labels.join(', ')}',
                    style: muted,
                  ),
                  Text(
                    r.maxConcurrent == null
                        ? busy
                        : '$busy · at most ${r.maxConcurrent}',
                    style: muted,
                  ),
                  Text(_seen(r), style: muted),
                ],
              ),
            ),
            const SizedBox(width: 8),
            // A leaf the demo script reads: its text is the status word.
            Semantics(
              label: 'runtime-status:${r.runnerId}',
              value: word,
              excludeSemantics: true,
              child: Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  Icon(icon, size: 14, color: colour),
                  const SizedBox(width: 4),
                  Text(
                    word,
                    style: text.labelSmall?.copyWith(
                      fontWeight: FontWeight.w600,
                    ),
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
