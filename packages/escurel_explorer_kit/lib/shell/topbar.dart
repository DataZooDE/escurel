import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../config/env.dart';
import '../config/feature_flags.dart';
import '../state/explorer_nav.dart';

class Topbar extends ConsumerWidget implements PreferredSizeWidget {
  const Topbar({super.key});

  @override
  Size get preferredSize => const Size.fromHeight(52);

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final env = ref.watch(envProvider);
    final text = Theme.of(context).textTheme;
    final cs = Theme.of(context).colorScheme;

    return AppBar(
      automaticallyImplyLeading: false,
      titleSpacing: 16,
      toolbarHeight: 52,
      title: Row(
        children: [
          Container(
            width: 28,
            height: 28,
            decoration: BoxDecoration(
              color: cs.primary,
              borderRadius: BorderRadius.circular(6),
            ),
            alignment: Alignment.center,
            child: Text(
              'e',
              style: text.titleMedium?.copyWith(color: cs.onPrimary),
            ),
          ),
          const SizedBox(width: 10),
          Text(
            'escurel-explore',
            style: text.titleMedium?.copyWith(color: cs.onSurface),
          ),
          const SizedBox(width: 12),
          _Chip(label: env.version, tone: _Tone.neutral),
          const Spacer(),
          if (!ref.watch(writeEnabledProvider))
            const Padding(
              padding: EdgeInsets.only(right: 8),
              child: _Chip(
                key: ValueKey('topbar.read_only_chip'),
                label: 'read-only',
                tone: _Tone.warning,
              ),
            ),
          // Standalone-only surfaces (reached by go_router). Hidden when
          // the shell is embedded in a host without a router.
          if (!ref.watch(explorerEmbeddedProvider)) ...[
            const _NavLink(
                label: 'CRM',
                icon: Icons.account_tree_outlined,
                to: '/crm',
                semantics: 'open-crm'),
            const SizedBox(width: 8),
            const _InspectorToggle(),
            const SizedBox(width: 8),
          ],
          _Chip(
            label: env.mode.name,
            tone: env.mode == AppMode.fixture ? _Tone.warning : _Tone.success,
          ),
        ],
      ),
    );
  }
}

class _InspectorToggle extends ConsumerWidget {
  const _InspectorToggle();

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final cs = Theme.of(context).colorScheme;
    return Tooltip(
      message: 'Open dev inspector',
      child: InkWell(
        key: const ValueKey('topbar.inspector_toggle'),
        borderRadius: BorderRadius.circular(6),
        onTap: () => explorerGo(context, ref, '/inspector'),
        child: Container(
          padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 4),
          decoration: BoxDecoration(
            color: cs.surfaceContainerHigh,
            borderRadius: BorderRadius.circular(6),
          ),
          child: Row(
            children: [
              Icon(Icons.terminal, size: 14, color: cs.onSurface),
              const SizedBox(width: 4),
              Text(
                'inspector',
                style: Theme.of(context)
                    .textTheme
                    .labelSmall
                    ?.copyWith(color: cs.onSurface),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

class _NavLink extends ConsumerWidget {
  const _NavLink({required this.label, required this.icon, required this.to, required this.semantics});
  final String label;
  final IconData icon;
  final String to;
  final String semantics;
  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final cs = Theme.of(context).colorScheme;
    return Tooltip(
      message: 'Open $label',
      child: InkWell(
        borderRadius: BorderRadius.circular(6),
        onTap: () => explorerGo(context, ref, to),
        child: Semantics(
          label: semantics,
          button: true,
          child: Container(
            padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 4),
            decoration: BoxDecoration(
              color: cs.surfaceContainerHigh,
              borderRadius: BorderRadius.circular(6),
            ),
            child: Row(
              children: [
                Icon(icon, size: 14, color: cs.onSurface),
                const SizedBox(width: 4),
                Text(label, style: Theme.of(context).textTheme.labelSmall?.copyWith(color: cs.onSurface)),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

enum _Tone { neutral, success, warning }

class _Chip extends StatelessWidget {
  const _Chip({super.key, required this.label, required this.tone});

  final String label;
  final _Tone tone;

  @override
  Widget build(BuildContext context) {
    final cs = Theme.of(context).colorScheme;
    final (bg, fg) = switch (tone) {
      _Tone.neutral => (cs.surfaceContainer, cs.onSurfaceVariant),
      _Tone.success => (cs.secondaryContainer, cs.onSecondaryContainer),
      _Tone.warning => (const Color(0xFFFFF1CC), const Color(0xFF5A3F00)),
    };
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 3),
      decoration: BoxDecoration(
        color: bg,
        borderRadius: BorderRadius.circular(999),
      ),
      child: Text(
        label,
        style: Theme.of(context).textTheme.labelSmall?.copyWith(color: fg),
      ),
    );
  }
}
