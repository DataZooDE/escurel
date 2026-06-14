/// Typed view of `--dart-define` values that configure the app at build time.
///
/// All values default to fixture mode so `flutter run -d chrome` works
/// without further flags. Override via `--dart-define=...` for HTTP mode.
library;

import 'package:flutter_riverpod/flutter_riverpod.dart';

/// The active environment. Defaults to the build-time defines; an
/// embedding host (e.g. an app that supplies its own client) overrides
/// this in its `ProviderScope` to label the mode/version it runs under.
final envProvider = Provider<Env>((ref) => Env.fromDefines());

class Env {
  const Env({
    required this.mode,
    required this.baseUrl,
    required this.auth,
    required this.version,
  });

  factory Env.fromDefines() {
    const mode = String.fromEnvironment('ESCUREL_EXPLORE_MODE', defaultValue: 'fixture');
    const baseUrl = String.fromEnvironment('ESCUREL_EXPLORE_BASE_URL', defaultValue: '');
    const auth = String.fromEnvironment('ESCUREL_EXPLORE_AUTH', defaultValue: 'none');
    const version = String.fromEnvironment('ESCUREL_EXPLORE_VERSION', defaultValue: 'dev');

    return Env(
      mode: AppMode.values.firstWhere(
        (m) => m.name == mode,
        orElse: () => AppMode.fixture,
      ),
      baseUrl: baseUrl,
      auth: AuthMode.values.firstWhere(
        (a) => a.name == auth,
        orElse: () => AuthMode.none,
      ),
      version: version,
    );
  }

  final AppMode mode;
  final String baseUrl;
  final AuthMode auth;
  final String version;
}

enum AppMode { fixture, http }

enum AuthMode { none, bearer, oidc }
