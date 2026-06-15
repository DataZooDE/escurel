/// Top breadcrumb for the CRM workspace: `data zoo / CRM`, the live
/// instance count, and the currently-focused entity. Mirrors the
/// mockup's top bar.
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../state/providers.dart';
import '../theme/app_theme.dart';
import 'auto_refresh.dart';
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
    final trail = ref.watch(navBackStackProvider);

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
              text: TextSpan(
                children: [
                  TextSpan(
                    text: 'data zoo',
                    style: text.titleMedium?.copyWith(
                      color: kOnSurface,
                      fontWeight: FontWeight.w700,
                    ),
                  ),
                  TextSpan(
                    text: '  /  CRM',
                    style: text.titleMedium?.copyWith(color: kOutline),
                  ),
                ],
              ),
            ),
          ),
          const SizedBox(width: 16),
          const InstancesMenu(),
          if (focused != null)
            // The history trail (ancestors as clickable crumbs) + the
            // focused entity, scrollable so a deep trail never overflows.
            Flexible(
              child: SingleChildScrollView(
                scrollDirection: Axis.horizontal,
                reverse: true, // keep the focused entity in view when long
                child: Row(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    for (var i = 0; i < trail.length; i++) ...[
                      _Sep(),
                      _TrailCrumb(
                        label: 'crumb:${_slug(trail[i])}',
                        text: _entityLabel(trail[i]),
                        onTap: () => navigateToDepth(ref, i),
                      ),
                    ],
                    _Sep(),
                    _Crumb(
                      label: 'focused-entity',
                      child: Text(
                        _entityLabel(focused),
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: text.labelLarge?.copyWith(
                          color: kPrimary,
                          fontWeight: FontWeight.w700,
                        ),
                      ),
                    ),
                  ],
                ),
              ),
            ),
        ],
      ),
      actions: const [
        _AutoRefreshToggle(),
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

  /// `…/engagement__hoffmann-spine.md` → `hoffmann-spine` (semantics key).
  static String _slug(String pageId) {
    final base = pageId.split('/').last.replaceAll('.md', '');
    final parts = base.split('__');
    return parts.length == 2 ? parts[1] : base;
  }
}

/// A clickable ancestor crumb in the history trail — tapping it jumps focus
/// back to that depth.
class _TrailCrumb extends StatelessWidget {
  const _TrailCrumb({
    required this.label,
    required this.text,
    required this.onTap,
  });
  final String label;
  final String text;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context).textTheme;
    return Semantics(
      label: label,
      button: true,
      onTap: onTap,
      excludeSemantics: true,
      child: InkWell(
        onTap: onTap,
        borderRadius: BorderRadius.circular(6),
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 4, vertical: 2),
          child: Text(
            text,
            maxLines: 1,
            overflow: TextOverflow.ellipsis,
            style: theme.labelLarge?.copyWith(color: kOnSurfaceVariant),
          ),
        ),
      ),
    );
  }
}

class _Crumb extends StatelessWidget {
  const _Crumb({required this.label, required this.child});
  final String label;
  final Widget child;
  @override
  Widget build(BuildContext context) => Semantics(
    label: label,
    container: true,
    explicitChildNodes: true,
    child: child,
  );
}

/// Pause/resume the knowledge-base auto-refresh. On by default; the icon
/// reflects the live state.
class _AutoRefreshToggle extends ConsumerWidget {
  const _AutoRefreshToggle();

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final on = ref.watch(autoRefreshEnabledProvider);
    return Semantics(
      label: 'auto-refresh-toggle',
      toggled: on,
      button: true,
      excludeSemantics: true,
      child: IconButton(
        tooltip: on ? 'Auto-Aktualisierung an' : 'Auto-Aktualisierung aus',
        icon: Icon(
          on ? Icons.sync : Icons.sync_disabled,
          size: 18,
          color: on ? kPrimary : kOutline,
        ),
        onPressed: () =>
            ref.read(autoRefreshEnabledProvider.notifier).state = !on,
      ),
    );
  }
}

class _Sep extends StatelessWidget {
  @override
  Widget build(BuildContext context) => const Padding(
    padding: EdgeInsets.symmetric(horizontal: 10),
    child: Icon(Icons.chevron_right, size: 16, color: kOutlineVariant),
  );
}
