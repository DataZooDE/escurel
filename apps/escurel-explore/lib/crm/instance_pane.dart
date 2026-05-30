/// RIGHT pane (M7) — the **instance view** of the focused memory: the
/// instance connected to its skills (the skill-wheel) over its
/// materialized state (and, via the time scrubber, its state over time).
library;

import 'package:flutter/material.dart';

import '../editor/entity_editor.dart';
import '../theme/app_theme.dart';
import 'skill_wheel.dart';

class InstancePane extends StatelessWidget {
  const InstancePane({super.key});

  @override
  Widget build(BuildContext context) {
    return Semantics(
      label: 'instance-pane',
      container: true,
      explicitChildNodes: true,
      child: const Column(
        children: [
          // The instance ↔ skills connection.
          SizedBox(height: 220, child: SkillWheel()),
          Divider(height: 1, color: kOutlineVariant),
          // The materialized state (re-materialized as-of T by the scrubber).
          Expanded(child: EntityEditor()),
        ],
      ),
    );
  }
}
