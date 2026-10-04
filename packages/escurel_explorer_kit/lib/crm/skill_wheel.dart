/// Radial "skill-wheel" link navigator — the distinctive widget from
/// the mockups. The focused entity sits at the hub; each typed
/// neighbour (from `neighbours(both)`) is a node on the ring, coloured
/// by its link skill. Tapping a node re-centres the workspace on that
/// entity. Pure composition over the `neighbours` tool — no backend
/// risk, only layout.
library;

import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../state/providers.dart';
import '../theme/app_theme.dart';
import '../widgets/skill_avatar.dart';
import 'crm_providers.dart';

/// Per-skill spoke colour — shared with the link chips + menus so a skill
/// reads the same everywhere.
Color _skillColor(String skill) => skillColor(skill);

class SkillWheel extends ConsumerWidget {
  const SkillWheel({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final focused = ref.watch(currentPageIdProvider);
    final page = ref.watch(currentPageProvider);
    final neighbours = ref.watch(currentNeighboursProvider);

    if (focused == null) {
      return const _WheelFrame(child: Center(child: Text('No entity focused')));
    }

    final hubLabel = page.maybeWhen(
      data: (p) => (p?.skill ?? '').toUpperCase(),
      orElse: () => '',
    );

    return neighbours.when(
      loading: () => const _WheelFrame(child: Center(child: CircularProgressIndicator(strokeWidth: 2))),
      error: (e, _) => _WheelFrame(child: Center(child: Text('wheel error: $e'))),
      data: (edges) {
        // Unique (linkSkill, dst) nodes; drop self-references.
        final seen = <String>{};
        final nodes = <_Node>[];
        for (final e in edges) {
          final key = '${e.linkSkill}::${e.dst}';
          if (e.dst.isEmpty || !seen.add(key)) continue;
          nodes.add(_Node(linkSkill: e.linkSkill, slug: e.dst));
        }
        return _WheelFrame(
          linkCount: nodes.length,
          child: LayoutBuilder(
            builder: (context, c) {
              final size = math.min(c.maxWidth, c.maxHeight);
              return SizedBox(
                width: size,
                height: size,
                child: _Radial(hubLabel: hubLabel, nodes: nodes, ref: ref),
              );
            },
          ),
        );
      },
    );
  }
}

class _Node {
  const _Node({required this.linkSkill, required this.slug});
  final String linkSkill;
  final String slug;
}

class _WheelFrame extends StatelessWidget {
  const _WheelFrame({required this.child, this.linkCount});
  final Widget child;
  final int? linkCount;
  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    return Semantics(
      label: 'skill-wheel',
      container: true,
      explicitChildNodes: true,
      child: Container(
        margin: const EdgeInsets.all(12),
        padding: const EdgeInsets.all(12),
        decoration: BoxDecoration(
          color: kSurfaceContainerLow,
          borderRadius: BorderRadius.circular(12),
          border: Border.all(color: kOutlineVariant),
        ),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              mainAxisAlignment: MainAxisAlignment.spaceBetween,
              children: [
                Text('NAVIGATOR', style: text.labelSmall?.copyWith(color: kOutline, letterSpacing: 1)),
                if (linkCount != null)
                  Text('$linkCount links', style: text.labelSmall?.copyWith(color: kOutline)),
              ],
            ),
            const SizedBox(height: 8),
            Expanded(child: child),
          ],
        ),
      ),
    );
  }
}

class _Radial extends StatelessWidget {
  const _Radial({required this.hubLabel, required this.nodes, required this.ref});
  final String hubLabel;
  final List<_Node> nodes;
  final WidgetRef ref;

  @override
  Widget build(BuildContext context) {
    return LayoutBuilder(
      builder: (context, c) {
        final w = c.maxWidth, h = c.maxHeight;
        final cx = w / 2, cy = h / 2;
        const node = 34.0; // node diameter
        final radius = math.max(24.0, math.min(w, h) / 2 - node);
        final positions = <Offset>[];
        for (var i = 0; i < nodes.length; i++) {
          final a = (nodes.isEmpty ? 0 : 2 * math.pi * i / nodes.length) - math.pi / 2;
          positions.add(Offset(cx + radius * math.cos(a), cy + radius * math.sin(a)));
        }
        return Stack(
          children: [
            // Spokes + ring.
            Positioned.fill(
              child: CustomPaint(
                painter: _SpokePainter(
                  center: Offset(cx, cy),
                  nodes: [for (var i = 0; i < nodes.length; i++) (positions[i], _skillColor(nodes[i].linkSkill))],
                  radius: radius,
                ),
              ),
            ),
            // Hub.
            Positioned(
              left: cx - 36,
              top: cy - 36,
              child: _Hub(label: hubLabel),
            ),
            // Nodes.
            for (var i = 0; i < nodes.length; i++)
              Positioned(
                left: positions[i].dx - node / 2,
                top: positions[i].dy - node / 2,
                width: node,
                height: node,
                child: _WheelNode(node: nodes[i], ref: ref),
              ),
          ],
        );
      },
    );
  }
}

class _Hub extends StatelessWidget {
  const _Hub({required this.label});
  final String label;
  @override
  Widget build(BuildContext context) => Container(
        width: 72,
        height: 72,
        alignment: Alignment.center,
        decoration: const BoxDecoration(color: kPrimary, shape: BoxShape.circle),
        child: Padding(
          padding: const EdgeInsets.all(4),
          child: FittedBox(
            child: Text(
              label.isEmpty ? '·' : label,
              style: Theme.of(context).textTheme.labelSmall?.copyWith(color: Colors.white, fontWeight: FontWeight.w700),
            ),
          ),
        ),
      );
}

class _WheelNode extends StatelessWidget {
  const _WheelNode({required this.node, required this.ref});
  final _Node node;
  final WidgetRef ref;
  @override
  Widget build(BuildContext context) {
    final color = _skillColor(node.linkSkill);
    return Semantics(
      label: 'wheel-node',
      button: true,
      onTap: () => focusWikilink(ref, node.linkSkill, node.slug),
      excludeSemantics: true,
      child: Tooltip(
        message: '${node.linkSkill}::${node.slug}',
        child: InkWell(
          customBorder: const CircleBorder(),
          onTap: () => focusWikilink(ref, node.linkSkill, node.slug),
          child: Container(
            alignment: Alignment.center,
            decoration: BoxDecoration(
              color: color.withValues(alpha: 0.16),
              shape: BoxShape.circle,
              border: Border.all(color: color, width: 1.5),
            ),
            child: Text(
              node.linkSkill.isEmpty ? '?' : node.linkSkill[0].toUpperCase(),
              style: Theme.of(context).textTheme.labelMedium?.copyWith(color: color, fontWeight: FontWeight.w700),
            ),
          ),
        ),
      ),
    );
  }
}

class _SpokePainter extends CustomPainter {
  _SpokePainter({required this.center, required this.nodes, required this.radius});
  final Offset center;
  final List<(Offset, Color)> nodes;
  final double radius;

  @override
  void paint(Canvas canvas, Size size) {
    final ring = Paint()
      ..style = PaintingStyle.stroke
      ..strokeWidth = 1
      ..color = kOutlineVariant;
    canvas.drawCircle(center, radius, ring);
    for (final (pos, color) in nodes) {
      final spoke = Paint()
        ..strokeWidth = 1.5
        ..color = color.withValues(alpha: 0.5);
      canvas.drawLine(center, pos, spoke);
    }
  }

  @override
  bool shouldRepaint(covariant _SpokePainter old) =>
      old.center != center || old.radius != radius || old.nodes.length != nodes.length;
}
