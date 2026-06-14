import 'package:flutter/material.dart';

import '../theme/app_theme.dart';

/// An anchored dropdown that hangs off a top-bar element (the ☰ skills
/// button, the Instances crumb). The trigger is built by [trigger] with
/// an `open` callback; [panelBuilder] supplies the panel contents and is
/// given a `close` callback. The panel floats in the [Overlay] (so it
/// isn't clipped by the AppBar) with a full-screen tap-to-dismiss barrier.
class BreadcrumbMenu extends StatefulWidget {
  const BreadcrumbMenu({
    super.key,
    required this.trigger,
    required this.panelBuilder,
    this.width = 340,
    this.maxHeight = 560,
  });

  final Widget Function(VoidCallback open) trigger;
  final Widget Function(VoidCallback close) panelBuilder;
  final double width;
  final double maxHeight;

  @override
  State<BreadcrumbMenu> createState() => _BreadcrumbMenuState();
}

class _BreadcrumbMenuState extends State<BreadcrumbMenu> {
  final _controller = OverlayPortalController();
  final _link = LayerLink();

  void _open() => _controller.show();
  void _close() => _controller.hide();

  @override
  Widget build(BuildContext context) {
    return CompositedTransformTarget(
      link: _link,
      child: OverlayPortal(
        controller: _controller,
        overlayChildBuilder: (context) => Stack(
          children: [
            // Tap-outside to dismiss.
            Positioned.fill(
              child: GestureDetector(
                behavior: HitTestBehavior.opaque,
                onTap: _close,
                child: const SizedBox.shrink(),
              ),
            ),
            CompositedTransformFollower(
              link: _link,
              showWhenUnlinked: false,
              targetAnchor: Alignment.bottomLeft,
              followerAnchor: Alignment.topLeft,
              offset: const Offset(0, 6),
              child: SizedBox(
                width: widget.width,
                child: Material(
                  elevation: 10,
                  borderRadius: BorderRadius.circular(10),
                  color: kSurfaceContainerLowest,
                  clipBehavior: Clip.antiAlias,
                  child: ConstrainedBox(
                    constraints: BoxConstraints(maxHeight: widget.maxHeight),
                    child: widget.panelBuilder(_close),
                  ),
                ),
              ),
            ),
          ],
        ),
        child: widget.trigger(_open),
      ),
    );
  }
}

/// Header band of a breadcrumb menu panel: a bold [title] + a muted
/// monospace-ish [subtitle].
class MenuHeader extends StatelessWidget {
  const MenuHeader({super.key, required this.title, required this.subtitle});
  final String title;
  final String subtitle;

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    return Container(
      width: double.infinity,
      padding: const EdgeInsets.fromLTRB(16, 12, 16, 12),
      decoration: const BoxDecoration(
        border: Border(bottom: BorderSide(color: kOutlineVariant)),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(title, style: text.titleSmall?.copyWith(color: kOnSurface, fontWeight: FontWeight.w700)),
          const SizedBox(height: 2),
          Text(subtitle, style: text.bodySmall?.copyWith(color: kOutline)),
        ],
      ),
    );
  }
}

/// A `SECTION · N` group label inside a breadcrumb menu panel.
class MenuSectionHeader extends StatelessWidget {
  const MenuSectionHeader({super.key, required this.label, required this.count});
  final String label;
  final int count;

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 14, 16, 6),
      child: Text(
        '$label · $count',
        style: text.labelSmall?.copyWith(color: kOutline, letterSpacing: 1),
      ),
    );
  }
}

/// Footer hint band of a breadcrumb menu panel.
class MenuFooter extends StatelessWidget {
  const MenuFooter({super.key, required this.hint});
  final String hint;

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    return Container(
      width: double.infinity,
      padding: const EdgeInsets.fromLTRB(16, 8, 16, 10),
      decoration: const BoxDecoration(
        border: Border(top: BorderSide(color: kOutlineVariant)),
      ),
      child: Text(hint, style: text.labelSmall?.copyWith(color: kOutline)),
    );
  }
}
