// An external backend (sql_view / document) is read-only: the server
// rejects `update_page` for any non-writable backend, so the explorer's
// edit/create affordance (driven by skillEditableProvider) must stay off
// for its instances even when write mode is on and the skill's ACL would
// otherwise mark it operator-editable.

import 'package:escurel_explorer_kit/client/models.dart';
import 'package:escurel_explorer_kit/config/feature_flags.dart';
import 'package:escurel_explorer_kit/state/providers.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

SkillSummary _skill(String id, String kind, SkillCapabilities caps) =>
    SkillSummary(
      id: id,
      description: '',
      requiredFrontmatter: const [],
      optionalFrontmatter: const [],
      backendKind: kind,
      capabilities: caps,
      // No acl + no ownerField → operatorEditable is true, so the only
      // thing that can flip editability off is the backend capability.
    );

void main() {
  test('non-writable backends are not operator-editable', () async {
    final container = ProviderContainer(
      overrides: [
        writeEnabledProvider.overrideWithValue(true),
        skillsCatalogueProvider.overrideWith(
          (ref) async => [
            _skill('note', 'markdown', const SkillCapabilities()),
            _skill(
              'erp_customer',
              'sql_view',
              const SkillCapabilities(writable: false),
            ),
            _skill(
              'contract',
              'document',
              const SkillCapabilities(writable: false),
            ),
          ],
        ),
      ],
    );
    addTearDown(container.dispose);

    // Resolve the catalogue future before reading the sync gate.
    await container.read(skillsCatalogueProvider.future);
    final editable = container.read(skillEditableProvider);

    // The markdown skill (writable) is editable; the external ones are not.
    expect(editable('note'), isTrue);
    expect(editable('erp_customer'), isFalse);
    expect(editable('contract'), isFalse);
  });
}
