import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../app.dart';
import '../config/env.dart';
import '../theme/app_theme.dart';

class Topbar extends ConsumerWidget implements PreferredSizeWidget {
  const Topbar({super.key});

  @override
  Size get preferredSize => const Size.fromHeight(52);

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final env = ref.watch(envProvider);
    final text = Theme.of(context).textTheme;

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
              color: kPrimary,
              borderRadius: BorderRadius.circular(6),
            ),
            alignment: Alignment.center,
            child: Text(
              'e',
              style: text.titleMedium?.copyWith(color: Colors.white),
            ),
          ),
          const SizedBox(width: 10),
          Text(
            'escurel-explore',
            style: text.titleMedium?.copyWith(color: kOnSurface),
          ),
          const SizedBox(width: 12),
          _Chip(label: env.version, tone: _Tone.neutral),
          const Spacer(),
          _Chip(
            label: env.mode.name,
            tone: env.mode == AppMode.fixture ? _Tone.warning : _Tone.success,
          ),
        ],
      ),
    );
  }
}

enum _Tone { neutral, success, warning }

class _Chip extends StatelessWidget {
  const _Chip({required this.label, required this.tone});

  final String label;
  final _Tone tone;

  @override
  Widget build(BuildContext context) {
    final (bg, fg) = switch (tone) {
      _Tone.neutral => (kSurfaceContainer, kOnSurfaceVariant),
      _Tone.success => (kSecondaryContainer, kOnSecondaryContainer),
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
