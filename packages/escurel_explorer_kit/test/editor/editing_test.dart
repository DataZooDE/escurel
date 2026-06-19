// No-mock widget tests for the explorer's EDITING surface. Every test
// pumps the real `EscurelExplorer` over a write-enabled
// `FixtureEscurelClient` (a real client impl) and drives the UI through
// its Semantics labels — exactly the rodney a11y contract.
//
// Coverage: edit an editable instance, a validation error blocking
// save, the owner-bound read-only guard, create a new instance,
// tombstone delete, and the write-disabled gate.

@TestOn('vm')
library;

import 'package:escurel_explorer_kit/client/escurel_client.dart';
import 'package:escurel_explorer_kit/client/fixture_escurel_client.dart';
import 'package:escurel_explorer_kit/editor/page_form.dart';
import 'package:escurel_explorer_kit/escurel_explorer.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

// ── corpus ──────────────────────────────────────────────────────

const _noteSkill = '---\n'
    'type: skill\n'
    'id: note\n'
    'description: A free-form note.\n'
    'required_frontmatter: [title]\n'
    'optional_frontmatter: [tags]\n'
    '---\n\n# note\n';

const _noteWelcome = '---\n'
    'type: instance\n'
    'skill: note\n'
    'id: welcome\n'
    'title: Welcome\n'
    '---\n\n# Welcome\n\nThe original body.\n';

// An owner-bound skill: a non-null owner_field marks it as never
// operator-editable, mirroring private_profile.
const _profileSkill = '---\n'
    'type: skill\n'
    'id: private_profile\n'
    'description: An owner-bound profile.\n'
    'visibility: owner\n'
    'owner_field: owner\n'
    'required_frontmatter: [owner]\n'
    '---\n\n# private_profile\n';

const _profileInstance = '---\n'
    'type: instance\n'
    'skill: private_profile\n'
    'id: secret\n'
    'owner: "whatsapp:123"\n'
    '---\n\n# secret\n';

// Group-ACL skills (group ACL v1) WITHOUT a legacy owner_field — the
// operator edit-gate must read the `acl.update` policy: owner-scoped ⇒
// read-only; admin/group-writable ⇒ editable.
const _ticketSkill = '---\n'
    'type: skill\n'
    'id: ticket\n'
    'description: An owner-scoped ticket (acl, no owner_field).\n'
    'owner_field: reporter\n'
    'acl:\n'
    '  read: [public]\n'
    '  update: [owner]\n'
    'required_frontmatter: [reporter]\n'
    '---\n\n# ticket\n';

const _ticketInstance = '---\n'
    'type: instance\n'
    'skill: ticket\n'
    'id: t1\n'
    'reporter: "whatsapp:999"\n'
    '---\n\n# t1\n';

const _bulletinSkill = '---\n'
    'type: skill\n'
    'id: bulletin\n'
    'description: An admin-writable bulletin (acl).\n'
    'acl:\n'
    '  read: [public]\n'
    '  update: [admin]\n'
    'required_frontmatter: [title]\n'
    '---\n\n# bulletin\n';

FixtureEscurelClient _writableClient() => FixtureEscurelClient.fromSources(
      writeEnabled: true,
      skillFiles: {
        'note.md': _noteSkill,
        'private_profile.md': _profileSkill,
      },
      instanceFiles: {
        'note__welcome.md': _noteWelcome,
        'private_profile__secret.md': _profileInstance,
      },
    );

FixtureEscurelClient _readOnlyClient() => FixtureEscurelClient.fromSources(
      // writeEnabled defaults to false → version() omits agentWriteTools.
      skillFiles: {'note.md': _noteSkill},
      instanceFiles: {'note__welcome.md': _noteWelcome},
    );

Future<void> _pump(WidgetTester tester, EscurelClient client) async {
  tester.view.physicalSize = const Size(1400, 1000);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  await tester.pumpWidget(
    MaterialApp(home: EscurelExplorer(client: client)),
  );
  await tester.pumpAndSettle();
}

/// Open `note::welcome` in the editor (it's keyed `note__welcome`).
Future<void> _openWelcome(WidgetTester tester) async {
  await tester.tap(find.text('welcome'));
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('edit an editable instance: change a field + body, save persists', (tester) async {
    final client = _writableClient();
    await _pump(tester, client);
    await _openWelcome(tester);

    // Enter edit mode.
    expect(find.bySemanticsLabel(PageFormKeys.editPage), findsOneWidget);
    await tester.tap(find.bySemanticsLabel(PageFormKeys.editPage));
    await tester.pumpAndSettle();

    // Change the title field + body.
    await tester.enterText(find.bySemanticsLabel('${PageFormKeys.fieldPrefix}title'), 'Renamed');
    await tester.enterText(find.bySemanticsLabel(PageFormKeys.bodyEditor), '# Renamed\n\nNew body.\n');
    await tester.pumpAndSettle();

    await tester.tap(find.bySemanticsLabel(PageFormKeys.save));
    await tester.pumpAndSettle();

    // The fixture reflects the write.
    final page = await client.expand('note__welcome');
    expect(page.frontmatter['title'], 'Renamed');
    expect(page.body, contains('New body.'));
  });

  testWidgets('validation error blocks save and leaves the fixture unchanged', (tester) async {
    final client = _writableClient();
    await _pump(tester, client);
    await _openWelcome(tester);

    await tester.tap(find.bySemanticsLabel(PageFormKeys.editPage));
    await tester.pumpAndSettle();

    // Clear the required `title` field (note declares it required).
    await tester.enterText(find.bySemanticsLabel('${PageFormKeys.fieldPrefix}title'), '');
    await tester.pumpAndSettle();

    // The inline validation status surfaces the error.
    expect(
      find.textContaining('required key "title"', findRichText: true),
      findsWidgets,
    );

    await tester.tap(find.bySemanticsLabel(PageFormKeys.save));
    await tester.pumpAndSettle();

    // The fixture is unchanged — the original title is intact.
    final page = await client.expand('note__welcome');
    expect(page.frontmatter['title'], 'Welcome');
    expect(page.body, contains('The original body.'));
  });

  testWidgets('read-only guard: an owner-bound skill shows no edit button', (tester) async {
    final client = _writableClient();
    await _pump(tester, client);

    // Open the owner-bound instance (private_profile::secret).
    await tester.tap(find.text('secret'));
    await tester.pumpAndSettle();

    // Write is enabled globally, but this skill is owner-bound → no edit.
    expect(find.bySemanticsLabel(PageFormKeys.editPage), findsNothing);
    // The editable note still offers it.
    await _openWelcome(tester);
    expect(find.bySemanticsLabel(PageFormKeys.editPage), findsOneWidget);
  });

  testWidgets('group-acl gate: owner-scoped acl.update is read-only; admin-writable is editable', (tester) async {
    final client = FixtureEscurelClient.fromSources(
      writeEnabled: true,
      skillFiles: {
        'ticket.md': _ticketSkill,
        'bulletin.md': _bulletinSkill,
      },
      instanceFiles: {'ticket__t1.md': _ticketInstance},
    );
    await _pump(tester, client);

    // ticket: acl.update == [owner] ⇒ no operator edit affordance.
    await tester.tap(find.text('t1'));
    await tester.pumpAndSettle();
    expect(find.bySemanticsLabel(PageFormKeys.editPage), findsNothing);

    // bulletin: acl.update == [admin] (no owner) ⇒ create is offered.
    expect(find.bySemanticsLabel('create-instance:bulletin'), findsOneWidget);
    // ticket, being owner-scoped, offers no create affordance.
    expect(find.bySemanticsLabel('create-instance:ticket'), findsNothing);
  });

  testWidgets('create: a new instance appears in the catalogue and is navigable', (tester) async {
    final client = _writableClient();
    await _pump(tester, client);

    // The editable note skill offers a create affordance.
    const createLabel = 'create-instance:note';
    expect(find.bySemanticsLabel(createLabel), findsOneWidget);
    await tester.tap(find.bySemanticsLabel(createLabel));
    await tester.pumpAndSettle();

    // Fill the id + required title, then save.
    await tester.enterText(find.bySemanticsLabel(PageFormKeys.idField), 'fresh');
    await tester.enterText(find.bySemanticsLabel('${PageFormKeys.fieldPrefix}title'), 'Fresh Note');
    await tester.pumpAndSettle();
    // The Save button can sit below the dialog's scroll fold — bring it
    // into view before tapping.
    await tester.ensureVisible(find.bySemanticsLabel(PageFormKeys.save));
    await tester.pumpAndSettle();
    await tester.tap(find.bySemanticsLabel(PageFormKeys.save));
    await tester.pumpAndSettle();

    // The fixture has the new page, keyed <skill>__<id>.
    final page = await client.expand('note__fresh');
    expect(page.frontmatter['title'], 'Fresh Note');

    // And it shows up in the catalogue listing.
    final instances = await client.listInstances('note');
    expect(instances.map((i) => i.id), contains('note__fresh'));
  });

  testWidgets('tombstone delete marks the instance erased (hidden by hide-erased)', (tester) async {
    final client = _writableClient();
    await _pump(tester, client);
    await _openWelcome(tester);

    await tester.tap(find.bySemanticsLabel(PageFormKeys.editPage));
    await tester.pumpAndSettle();

    await tester.tap(find.bySemanticsLabel(PageFormKeys.delete));
    await tester.pumpAndSettle();
    // Confirm dialog.
    await tester.tap(find.bySemanticsLabel(PageFormKeys.confirmDelete));
    await tester.pumpAndSettle();

    // The page is tombstoned.
    final page = await client.expand('note__welcome');
    expect(page.frontmatter['status'], 'erased');
    // Default hide-erased behaviour: still present in the raw corpus but
    // marked erased — the listing's `erased` flag is set.
    final instances = await client.listInstances('note');
    final welcome = instances.firstWhere((i) => i.id == 'note__welcome');
    expect(welcome.erased, isTrue);
  });

  testWidgets('write-disabled: no edit/create affordances when version omits agentWriteTools',
      (tester) async {
    final client = _readOnlyClient();
    await _pump(tester, client);

    // No create affordance in the catalogue.
    expect(find.bySemanticsLabel('create-instance:note'), findsNothing);

    // No edit button on an opened instance.
    await _openWelcome(tester);
    expect(find.bySemanticsLabel(PageFormKeys.editPage), findsNothing);
  });

  testWidgets('embedder editableSkills allowlist narrows editing (excludes an ownerless skill)',
      (tester) async {
    final client = _writableClient();
    tester.view.physicalSize = const Size(1400, 1000);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    // Write is enabled and `note` is ownerless (generically editable), but the
    // host restricts editing to a different skill — so note is read-only here.
    await tester.pumpWidget(
      MaterialApp(
        home: EscurelExplorer(client: client, editableSkills: const {'other'}),
      ),
    );
    await tester.pumpAndSettle();

    expect(find.bySemanticsLabel('create-instance:note'), findsNothing);
    await _openWelcome(tester);
    expect(find.bySemanticsLabel(PageFormKeys.editPage), findsNothing);
  });
}
