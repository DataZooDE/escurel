import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../theme/app_theme.dart';

/// Wraps a tappable reference (a wikilink pill, an event tile) with the
/// instance ↔ skill **dual** the triad implies — every reference is an
/// instance *of a skill*:
///
/// - **default tap** → [onPrimary] (the instance / open the event);
/// - **shift-click** → [onSkill] (the connected skill);
/// - **long hover** (~700 ms) → reveals a small clickable `→ <skill>`
///   chip just above the child; clicking it calls [onSkill]. Hovering
///   alone never navigates (the affordance is opt-in, not a surprise).
///
/// The chip floats in the [Overlay] (via [OverlayPortal]) so it never
/// reflows the document or gets clipped, and a short hide-grace lets the
/// pointer bridge from the child onto the chip.
class InstanceSkillLink extends StatefulWidget {
  const InstanceSkillLink({
    super.key,
    required this.child,
    required this.onPrimary,
    required this.onSkill,
    required this.skillLabel,
    this.borderRadius,
  });

  final Widget child;
  final VoidCallback onPrimary;
  final VoidCallback onSkill;

  /// The connected skill's id (e.g. `customer`, `meeting`) — shown in the
  /// hover chip and used as the semantics handle `goto-skill:<label>`.
  final String skillLabel;
  final BorderRadius? borderRadius;

  @override
  State<InstanceSkillLink> createState() => _InstanceSkillLinkState();
}

class _InstanceSkillLinkState extends State<InstanceSkillLink> {
  final _portal = OverlayPortalController();
  final _link = LayerLink();
  Timer? _dwell;
  Timer? _hide;

  static const _dwellDuration = Duration(milliseconds: 700);
  static const _hideGrace = Duration(milliseconds: 180);

  @override
  void dispose() {
    _dwell?.cancel();
    _hide?.cancel();
    super.dispose();
  }

  void _onEnterChild() {
    _hide?.cancel();
    _dwell?.cancel();
    _dwell = Timer(_dwellDuration, () {
      if (mounted) _portal.show();
    });
  }

  void _scheduleHide() {
    _dwell?.cancel();
    _hide?.cancel();
    _hide = Timer(_hideGrace, () {
      if (mounted) _portal.hide();
    });
  }

  void _tap() {
    if (HardwareKeyboard.instance.isShiftPressed) {
      widget.onSkill();
    } else {
      widget.onPrimary();
    }
  }

  void _tapSkillChip() {
    _portal.hide();
    widget.onSkill();
  }

  @override
  Widget build(BuildContext context) {
    return CompositedTransformTarget(
      link: _link,
      child: OverlayPortal(
        controller: _portal,
        overlayChildBuilder: _buildChip,
        child: MouseRegion(
          onEnter: (_) => _onEnterChild(),
          onExit: (_) => _scheduleHide(),
          child: InkWell(
            borderRadius: widget.borderRadius,
            onTap: _tap,
            child: widget.child,
          ),
        ),
      ),
    );
  }

  Widget _buildChip(BuildContext context) {
    final text = Theme.of(context).textTheme;
    // Align loosens the Overlay's tight (full-screen) constraints so the
    // follower can shrink-wrap the chip; the follower then anchors it just
    // above the child.
    return Align(
      alignment: Alignment.topLeft,
      child: CompositedTransformFollower(
        link: _link,
        showWhenUnlinked: false,
        targetAnchor: Alignment.topLeft,
        followerAnchor: Alignment.bottomLeft,
        offset: const Offset(0, -4),
        child: MouseRegion(
          onEnter: (_) => _hide?.cancel(),
          onExit: (_) => _scheduleHide(),
          child: Semantics(
            label: 'goto-skill:${widget.skillLabel}',
            button: true,
            onTap: _tapSkillChip,
            excludeSemantics: true,
            child: Material(
              type: MaterialType.transparency,
              child: InkWell(
                borderRadius: BorderRadius.circular(999),
                onTap: _tapSkillChip,
                child: Container(
                  padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 3),
                  decoration: BoxDecoration(
                    color: kSecondaryContainer,
                    borderRadius: BorderRadius.circular(999),
                    border: Border.all(color: kPrimary.withValues(alpha: 0.45)),
                    boxShadow: const [
                      BoxShadow(color: Color(0x22000000), blurRadius: 6, offset: Offset(0, 2)),
                    ],
                  ),
                  child: Row(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      const Icon(Icons.arrow_forward, size: 12, color: kOnSecondaryContainer),
                      const SizedBox(width: 4),
                      Text(
                        widget.skillLabel,
                        style: text.labelSmall
                            ?.copyWith(color: kOnSecondaryContainer, fontWeight: FontWeight.w600),
                      ),
                    ],
                  ),
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }
}
