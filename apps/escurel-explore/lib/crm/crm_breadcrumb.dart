/// Top breadcrumb for the CRM workspace: `data zoo / CRM`, the live
/// instance count, and the currently-focused entity. Mirrors the
/// mockup's top bar.
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../state/providers.dart';
import '../theme/app_theme.dart';
import 'instances_menu.dart';
import 'scenario_switch.dart';
import 'skills_menu.dart';

class CrmBreadcrumb extends ConsumerWidget implements PreferredSizeWidget {
  const CrmBreadcrumb({super.key});

  @override
  Size get preferredSize => const Size.fromHeight(52);

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final text = Theme.of(context).textTheme;
    final focused = ref.watch(currentPageIdProvider);

    return AppBar(
      automaticallyImplyLeading: false,
      toolbarHeight: 52,
      titleSpacing: 4,
      leadingWidth: 52,
      leading: const Center(child: SkillsMenu()),
      backgroundColor: kSurfaceContainerLowest,
      title: Row(
        children: [
          Semantics(
            label: 'brand',
            container: true,
            explicitChildNodes: true,
            child: RichText(
              text: TextSpan(children: [
                TextSpan(
                  text: 'data zoo',
                  style: text.titleMedium?.copyWith(color: kOnSurface, fontWeight: FontWeight.w700),
                ),
                TextSpan(
                  text: '  /  CRM',
                  style: text.titleMedium?.copyWith(color: kOutline),
                ),
              ]),
            ),
          ),
          const SizedBox(width: 16),
          const InstancesMenu(),
          if (focused != null) ...[
            _Sep(),
            _Crumb(
              label: 'focused-entity',
              child: Text(
                _entityLabel(focused),
                style: text.labelLarge?.copyWith(color: kPrimary, fontWeight: FontWeight.w700),
              ),
            ),
          ],
        ],
      ),
      actions: const [
        ScenarioSwitch(),
        SizedBox(width: 16),
      ],
    );
  }

  /// `markdown/instances/engagement__hoffmann-spine.md` → `engagement · hoffmann-spine`.
  static String _entityLabel(String pageId) {
    final base = pageId.split('/').last.replaceAll('.md', '');
    final parts = base.split('__');
    return parts.length == 2 ? '${parts[0]} · ${parts[1]}' : base;
  }
}

class _Crumb extends StatelessWidget {
  const _Crumb({required this.label, required this.child});
  final String label;
  final Widget child;
  @override
  Widget build(BuildContext context) =>
      Semantics(label: label, container: true, explicitChildNodes: true, child: child);
}

class _Sep extends StatelessWidget {
  @override
  Widget build(BuildContext context) => const Padding(
        padding: EdgeInsets.symmetric(horizontal: 10),
        child: Icon(Icons.chevron_right, size: 16, color: kOutlineVariant),
      );
}
