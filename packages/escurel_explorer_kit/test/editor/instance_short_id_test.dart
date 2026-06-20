// Unit test for the catalogue's instance label derivation. The real
// backend hands back page ids as paths (`markdown/instances/<skill>/<slug>.md`);
// the fixture client uses the `<skill>__<slug>` convention. Both must
// collapse to the bare slug in the catalogue list — the full path is noise.

@TestOn('vm')
library;

import 'package:escurel_explorer_kit/editor/catalogue_pane.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  test('backend path id → bare slug', () {
    expect(
      instanceShortLabel('markdown/instances/talk/abschlussrunde.md'),
      'abschlussrunde',
    );
    expect(
      instanceShortLabel('markdown/instances/talk/ki-aus-staatlicher-perspektive.md'),
      'ki-aus-staatlicher-perspektive',
    );
  });

  test('fixture `skill__slug` id → bare slug', () {
    expect(instanceShortLabel('note__welcome'), 'welcome');
  });

  test('a bare slug is returned unchanged', () {
    expect(instanceShortLabel('welcome'), 'welcome');
  });
}
