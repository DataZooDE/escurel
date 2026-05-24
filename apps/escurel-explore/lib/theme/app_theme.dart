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

ThemeData buildLightTheme() {
  const scheme = ColorScheme.light(
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

  final textTheme = _buildTextTheme(scheme);

  return ThemeData(
    useMaterial3: true,
    colorScheme: scheme,
    textTheme: textTheme,
    scaffoldBackgroundColor: kSurface,
    appBarTheme: AppBarTheme(
      backgroundColor: kSurfaceContainerLowest,
      foregroundColor: kOnSurface,
      elevation: 0,
      shadowColor: kOutlineVariant,
      titleTextStyle: textTheme.titleMedium,
    ),
    cardTheme: CardThemeData(
      color: kSurfaceContainerLowest,
      elevation: 0,
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(12),
        side: const BorderSide(color: kOutlineVariant),
      ),
    ),
    chipTheme: ChipThemeData(
      backgroundColor: kSurfaceContainerLow,
      labelStyle: textTheme.labelSmall,
      padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
    ),
    dividerTheme: const DividerThemeData(color: kOutlineVariant, thickness: 1),
    inputDecorationTheme: InputDecorationTheme(
      filled: true,
      fillColor: kSurfaceContainerLowest,
      border: OutlineInputBorder(
        borderRadius: BorderRadius.circular(8),
        borderSide: const BorderSide(color: kOutlineVariant),
      ),
      enabledBorder: OutlineInputBorder(
        borderRadius: BorderRadius.circular(8),
        borderSide: const BorderSide(color: kOutlineVariant),
      ),
    ),
  );
}

ThemeData buildDarkTheme() {
  const scheme = ColorScheme.dark(
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

  final textTheme = _buildTextTheme(scheme);

  return ThemeData(
    useMaterial3: true,
    colorScheme: scheme,
    textTheme: textTheme,
    scaffoldBackgroundColor: kDarkSurface,
    appBarTheme: AppBarTheme(
      backgroundColor: kDarkSurfaceContainerLow,
      foregroundColor: kDarkOnSurface,
      elevation: 0,
      titleTextStyle: textTheme.titleMedium,
    ),
    cardTheme: CardThemeData(
      color: kDarkSurfaceContainerLow,
      elevation: 0,
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(12),
        side: const BorderSide(color: kDarkOutlineVariant),
      ),
    ),
    dividerTheme: const DividerThemeData(color: kDarkOutlineVariant, thickness: 1),
  );
}
