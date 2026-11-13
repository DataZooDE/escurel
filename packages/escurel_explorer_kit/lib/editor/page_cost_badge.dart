/// What the runs that targeted a page cost over the last 30 days, shown in
/// the page header ("$0.42 · 7 runs (30d)"). Hidden — never an error — for
/// a non-admin (usage_report is admin-only), while loading, on a refused or
/// failed report, and for a page no run targeted.
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../client/models.dart';
import '../state/usage.dart';
import '../theme/app_theme.dart';

class PageCostBadge extends ConsumerWidget {
  const PageCostBadge({super.key, required this.pageId});

  final String pageId;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final row = ref.watch(usageByProvider('page')).asData?.value?[pageId];
    if (row == null || row.runs == 0) return const SizedBox.shrink();
    final label = costLabel(row);
    return Semantics(
      label: 'page-cost-badge',
      value: label,
      container: true,
      excludeSemantics: true,
      child: Tooltip(
        message: '${row.inputTokens} input / ${row.outputTokens} output tokens',
        child: Container(
          padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 3),
          decoration: BoxDecoration(
            color: kSurfaceContainerHigh,
            borderRadius: BorderRadius.circular(6),
          ),
          child: Text(
            label,
            style: Theme.of(context).textTheme.labelSmall?.copyWith(
              color: kOnSurfaceVariant,
              fontWeight: FontWeight.w600,
            ),
          ),
        ),
      ),
    );
  }
}

/// "$0.42 · 7 runs (30d)" — shared with the Agents panel so both read alike.
String costLabel(UsageRow row) =>
    '\$${row.costUsd.toStringAsFixed(2)} · ${row.runs} '
    '${row.runs == 1 ? 'run' : 'runs'} ($usageWindowLabel)';
