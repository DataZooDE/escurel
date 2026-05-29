import 'package:flutter/material.dart';
import 'package:flutter/semantics.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import 'app.dart';

void main() {
  WidgetsFlutterBinding.ensureInitialized();
  // Force-enable the semantics (accessibility) tree at startup.
  // Flutter web renders to a CanvasKit <canvas> with no
  // CSS-selectable DOM, so the browser-verification harness (rodney)
  // reaches the UI only through the a11y tree (`ax-find --name
  // <label>`). Without this, semantics stay off until assistive tech
  // is detected. The handle is intentionally never released — the
  // tree stays live for the whole session. See CLAUDE.md §"Demo app
  // + browser verification (rodney)".
  SemanticsBinding.instance.ensureSemantics();
  runApp(const ProviderScope(child: EscurelExploreApp()));
}
