import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import 'config/env.dart';
import 'shell/app_shell.dart';
import 'theme/app_theme.dart';

final envProvider = Provider<Env>((ref) => Env.fromDefines());

class EscurelExploreApp extends ConsumerWidget {
  const EscurelExploreApp({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    return MaterialApp(
      title: 'escurel-explore',
      debugShowCheckedModeBanner: false,
      theme: buildLightTheme(),
      darkTheme: buildDarkTheme(),
      themeMode: ThemeMode.light,
      home: const AppShell(),
    );
  }
}
