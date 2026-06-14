/// escurel_explorer_kit — reusable, stylable Flutter components for
/// exploring an Escurel knowledge base.
///
/// The shared core behind `escurel-explore` and any embedding host
/// (e.g. an operator dashboard). Hosts typically use [EscurelExplorer]
/// with their own [EscurelClient]; the lower-level widgets, providers,
/// client interface, and theme are exported for finer-grained use.
library;

// The one-stop embeddable widget.
export 'escurel_explorer.dart';

// Client interface, models, errors, and the two reference impls.
export 'client/escurel_client.dart';
export 'client/models.dart';
export 'client/errors.dart';
export 'client/http_escurel_client.dart';
export 'client/fixture_escurel_client.dart';

// Build-time environment + the overridable provider.
export 'config/env.dart';
export 'config/feature_flags.dart';

// Theme tokens + builders (the default "Analytical Atelier" palette).
export 'theme/app_theme.dart';

// Riverpod seams a host may override or watch.
export 'state/providers.dart';
export 'state/explorer_nav.dart';

// The exploration surfaces, for hosts that compose their own shell.
export 'shell/app_shell.dart';
export 'crm/crm_workspace.dart';
export 'editor/entity_editor.dart';
export 'editor/catalogue_pane.dart';
export 'inspector/inspector_shell.dart';
export 'demo/demo_screen.dart';

// Markdown helpers (frontmatter + wikilink parsing).
export 'md/frontmatter.dart';
export 'md/wikilink.dart';
