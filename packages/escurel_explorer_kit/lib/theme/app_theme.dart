import 'package:flutter/material.dart';
import 'package:google_fonts/google_fonts.dart';

// Design tokens — "Analytical Atelier" palette, mirrored from
// herkules-ui/lib/theme/app_theme.dart. Keep in sync until the shared
// datazoo_design_system package exists (deferred until a 3rd consumer).

const kPrimary = Color(0xFF004149);
const kPrimaryContainer = Color(0xFF1D5962);
const kOnPrimaryContainer = Color(0xFFCCE8EC);
const kSecondary = Color(0xFF006A61);
const kSecondaryContainer = Color(0xFF9DF2E6);
const kOnSecondaryContainer = Color(0xFF00201D);
const kPrimaryFixed = Color(0xFFBFE9EE);
const kOnPrimaryFixed = Color(0xFF001F23);

const kSurface = Color(0xFFF7F9FF);
const kSurfaceContainerLowest = Color(0xFFFFFFFF);
const kSurfaceContainerLow = Color(0xFFEDF4FF);
const kSurfaceContainer = Color(0xFFE5EEFC);
const kSurfaceContainerHigh = Color(0xFFD9EAFF);
const kSurfaceContainerHighest = Color(0xFFC7DCF4);
const kSurfaceVariant = Color(0xFFDCE4E8);

const kOnSurface = Color(0xFF091D2E);
const kOnSurfaceVariant = Color(0xFF40484D);
const kOutline = Color(0xFF70787D);
const kOutlineVariant = Color(0xFFC0C8CD);

const kError = Color(0xFFBA1A1A);
const kWarning = Color(0xFF9A6700);
const kSuccess = Color(0xFF2E7D5C);
const kInfo = Color(0xFF0B5E8F);

const kDarkSurface = Color(0xFF0D1B2A);
const kDarkSurfaceContainerLow = Color(0xFF1A2735);
const kDarkSurfaceContainer = Color(0xFF1E2E3E);
const kDarkOnSurface = Color(0xFFD6E4F0);
const kDarkOnSurfaceVariant = Color(0xFF9BAFBE);
const kDarkOutlineVariant = Color(0xFF3A4A56);

/// The kit's semantic colors that have no Material `ColorScheme` role
/// (`success`/`warning`/`info`), carried as a `ThemeExtension` so a host
/// can restyle them alongside the scheme. Read via
/// `Theme.of(context).explorerColors`. Defaults to the kit palette.
@immutable
class ExplorerColors extends ThemeExtension<ExplorerColors> {
  const ExplorerColors({
    required this.success,
    required this.warning,
    required this.info,
  });

  const ExplorerColors.light()
      : success = kSuccess,
        warning = kWarning,
        info = kInfo;

  const ExplorerColors.dark()
      : success = const Color(0xFF7FD1A8),
        warning = const Color(0xFFE0B65C),
        info = const Color(0xFF7FB8E0);

  final Color success;
  final Color warning;
  final Color info;

  @override
  ExplorerColors copyWith({Color? success, Color? warning, Color? info}) =>
      ExplorerColors(
        success: success ?? this.success,
        warning: warning ?? this.warning,
        info: info ?? this.info,
      );

  @override
  ExplorerColors lerp(ThemeExtension<ExplorerColors>? other, double t) {
    if (other is! ExplorerColors) return this;
    return ExplorerColors(
      success: Color.lerp(success, other.success, t)!,
      warning: Color.lerp(warning, other.warning, t)!,
      info: Color.lerp(info, other.info, t)!,
    );
  }
}

/// Convenience accessor: the kit's semantic colors for this theme,
/// falling back to the light defaults if a host theme didn't register
/// the extension.
extension ExplorerColorsX on ThemeData {
  ExplorerColors get explorerColors =>
      extension<ExplorerColors>() ?? const ExplorerColors.light();
}

TextTheme _buildTextTheme(ColorScheme scheme) {
  return GoogleFonts.latoTextTheme(
    TextTheme(
      displayLarge: GoogleFonts.arvo(fontSize: 32, fontWeight: FontWeight.w700),
      displayMedium: GoogleFonts.arvo(fontSize: 24, fontWeight: FontWeight.w700),
      titleLarge: GoogleFonts.lato(fontSize: 18, fontWeight: FontWeight.w700),
      titleMedium: GoogleFonts.lato(fontSize: 15, fontWeight: FontWeight.w700),
      titleSmall: GoogleFonts.lato(fontSize: 13, fontWeight: FontWeight.w700),
      bodyLarge: GoogleFonts.lato(fontSize: 15, fontWeight: FontWeight.w400),
      bodyMedium: GoogleFonts.lato(fontSize: 13, fontWeight: FontWeight.w400),
      bodySmall: GoogleFonts.lato(fontSize: 11, fontWeight: FontWeight.w400),
      labelLarge: GoogleFonts.lato(fontSize: 13, fontWeight: FontWeight.w700, letterSpacing: 0.5),
      labelSmall: GoogleFonts.lato(fontSize: 10, fontWeight: FontWeight.w400, letterSpacing: 0.5),
    ),
  );
}

const _defaultLightScheme = ColorScheme.light(
  primary: kPrimary,
  onPrimary: Colors.white,
  primaryContainer: kPrimaryContainer,
  onPrimaryContainer: kOnPrimaryContainer,
  secondary: kSecondary,
  onSecondary: Colors.white,
  secondaryContainer: kSecondaryContainer,
  surface: kSurface,
  onSurface: kOnSurface,
  onSurfaceVariant: kOnSurfaceVariant,
  outline: kOutline,
  outlineVariant: kOutlineVariant,
  error: kError,
  surfaceContainerLowest: kSurfaceContainerLowest,
  surfaceContainerLow: kSurfaceContainerLow,
  surfaceContainer: kSurfaceContainer,
  surfaceContainerHigh: kSurfaceContainerHigh,
  surfaceContainerHighest: kSurfaceContainerHighest,
);

/// The kit's light theme. A host may pass its own [colorScheme] and/or
/// [colors] (semantic extension) to restyle the explorer; both default
/// to the kit's "Analytical Atelier" palette, so the zero-arg call is
/// byte-identical to before. Chrome (app bar, cards, inputs) is derived
/// from the scheme, so an injected palette actually takes effect.
ThemeData buildLightTheme({ColorScheme? colorScheme, ExplorerColors? colors}) {
  final scheme = colorScheme ?? _defaultLightScheme;
  final textTheme = _buildTextTheme(scheme);

  return ThemeData(
    useMaterial3: true,
    colorScheme: scheme,
    textTheme: textTheme,
    extensions: [colors ?? const ExplorerColors.light()],
    scaffoldBackgroundColor: scheme.surface,
    appBarTheme: AppBarTheme(
      backgroundColor: scheme.surfaceContainerLowest,
      foregroundColor: scheme.onSurface,
      elevation: 0,
      shadowColor: scheme.outlineVariant,
      titleTextStyle: textTheme.titleMedium,
    ),
    cardTheme: CardThemeData(
      color: scheme.surfaceContainerLowest,
      elevation: 0,
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(12),
        side: BorderSide(color: scheme.outlineVariant),
      ),
    ),
    chipTheme: ChipThemeData(
      backgroundColor: scheme.surfaceContainerLow,
      labelStyle: textTheme.labelSmall,
      padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
    ),
    dividerTheme: DividerThemeData(color: scheme.outlineVariant, thickness: 1),
    inputDecorationTheme: InputDecorationTheme(
      filled: true,
      fillColor: scheme.surfaceContainerLowest,
      border: OutlineInputBorder(
        borderRadius: BorderRadius.circular(8),
        borderSide: BorderSide(color: scheme.outlineVariant),
      ),
      enabledBorder: OutlineInputBorder(
        borderRadius: BorderRadius.circular(8),
        borderSide: BorderSide(color: scheme.outlineVariant),
      ),
    ),
  );
}

const _defaultDarkScheme = ColorScheme.dark(
  primary: kOnPrimaryContainer,
  onPrimary: kPrimary,
  primaryContainer: kPrimaryContainer,
  secondary: kSecondaryContainer,
  surface: kDarkSurface,
  onSurface: kDarkOnSurface,
  onSurfaceVariant: kDarkOnSurfaceVariant,
  outline: Color(0xFF5A6E7E),
  outlineVariant: kDarkOutlineVariant,
  error: Color(0xFFFFB4AB),
  surfaceContainerLow: kDarkSurfaceContainerLow,
  surfaceContainer: kDarkSurfaceContainer,
);

/// The kit's dark theme. See [buildLightTheme] for the override seam.
ThemeData buildDarkTheme({ColorScheme? colorScheme, ExplorerColors? colors}) {
  final scheme = colorScheme ?? _defaultDarkScheme;
  final textTheme = _buildTextTheme(scheme);

  return ThemeData(
    useMaterial3: true,
    colorScheme: scheme,
    textTheme: textTheme,
    extensions: [colors ?? const ExplorerColors.dark()],
    scaffoldBackgroundColor: scheme.surface,
    appBarTheme: AppBarTheme(
      backgroundColor: scheme.surfaceContainerLow,
      foregroundColor: scheme.onSurface,
      elevation: 0,
      titleTextStyle: textTheme.titleMedium,
    ),
    cardTheme: CardThemeData(
      color: scheme.surfaceContainerLow,
      elevation: 0,
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(12),
        side: BorderSide(color: scheme.outlineVariant),
      ),
    ),
    dividerTheme: DividerThemeData(color: scheme.outlineVariant, thickness: 1),
  );
}
