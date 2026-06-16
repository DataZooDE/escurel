import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import 'client/escurel_client.dart';
import 'config/env.dart';
import 'shell/app_shell.dart';
import 'state/explorer_nav.dart';
import 'state/providers.dart';
import 'theme/app_theme.dart';

/// A complete, embeddable Escurel exploration environment: the
/// catalogue (skills → instances), the page view (frontmatter + body +
/// wikilinks), search, neighbour/wikilink navigation, and the event
/// rail — the same surface escurel-explore ships, packaged for any host.
///
/// The host supplies and owns the [client] (an HTTP client against a
/// gateway, a proxy, or a fixture) and its lifecycle. An optional
/// [theme] restyles `Theme.of`-based widgets; an optional [env] sets the
/// mode/version chips.
///
/// The explorer runs in its own isolated Riverpod [ProviderContainer]
/// (via [UncontrolledProviderScope]): the injected client is a *root*
/// override there, so it composes cleanly inside a host app that already
/// uses Riverpod — no scoped-provider plumbing, no leaking the host's
/// providers in or the kit's out. Render it under a `MaterialApp` (the
/// host's).
class EscurelExplorer extends StatefulWidget {
  const EscurelExplorer({
    super.key,
    required this.client,
    this.theme,
    this.env,
    this.editableSkills,
  });

  /// The backend the explorer talks to. The host owns construction and
  /// lifecycle (the explorer never closes a client it did not create).
  final EscurelClient client;

  /// Optional Material theme. Defaults to the kit's light theme; pass a
  /// host `ThemeData` to match the surrounding app.
  final ThemeData? theme;

  /// Optional environment label (drives the mode/version chips).
  /// Defaults to a neutral "live" label so an embedded explorer does not
  /// read as the standalone fixture build.
  final Env? env;

  /// Optional allowlist NARROWING which skills are operator-editable, to
  /// match a host's server-side write policy. `null` = no extra restriction
  /// (the generic ownerless rule applies). See [editableSkillsProvider].
  final Set<String>? editableSkills;

  static const Env _embeddedEnv = Env(
    mode: AppMode.http,
    baseUrl: '',
    auth: AuthMode.none,
    version: 'embedded',
  );

  @override
  State<EscurelExplorer> createState() => _EscurelExplorerState();
}

class _EscurelExplorerState extends State<EscurelExplorer> {
  late ProviderContainer _container;

  @override
  void initState() {
    super.initState();
    _container = _build();
  }

  ProviderContainer _build() => ProviderContainer(
        overrides: [
          escurelClientProvider.overrideWithValue(widget.client),
          envProvider.overrideWithValue(widget.env ?? EscurelExplorer._embeddedEnv),
          // Embedded: no go_router. Hide the standalone-only chrome (CRM,
          // dev-inspector) and route any stray navigation through a no-op.
          // In-page navigation (catalogue, wikilinks) is state-based and
          // unaffected.
          explorerEmbeddedProvider.overrideWithValue(true),
          explorerNavigateProvider.overrideWithValue((_) {}),
          editableSkillsProvider.overrideWithValue(widget.editableSkills),
        ],
      );

  @override
  void didUpdateWidget(EscurelExplorer old) {
    super.didUpdateWidget(old);
    // The client is identity-injected; rebuild the container if the host
    // swaps it (e.g. a fresh operator token mints a new client).
    if (!identical(old.client, widget.client) || old.env != widget.env) {
      final previous = _container;
      _container = _build();
      previous.dispose();
    }
  }

  @override
  void dispose() {
    _container.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return UncontrolledProviderScope(
      container: _container,
      child: Theme(
        data: widget.theme ?? buildLightTheme(),
        child: const AppShell(),
      ),
    );
  }
}
