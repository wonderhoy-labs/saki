package com.wonderhoylabs.saki.ui

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color

// Taken from the logo: its magenta line work, cyan accents and lemon yellow.
private val Magenta = Color(0xFFD6249B)
private val MagentaLight = Color(0xFFFF8BD6)
private val Cyan = Color(0xFF008FB3)
private val CyanLight = Color(0xFF6FE0FF)
private val Lemon = Color(0xFF7A6500)
private val LemonLight = Color(0xFFF5DE5C)

private val LightColors = lightColorScheme(
    primary = Magenta,
    onPrimary = Color.White,
    primaryContainer = Color(0xFFFFDDF2),
    onPrimaryContainer = Color(0xFF53003B),
    secondary = Cyan,
    onSecondary = Color.White,
    secondaryContainer = Color(0xFFD4F4FF),
    onSecondaryContainer = Color(0xFF00394A),
    tertiary = Lemon,
    tertiaryContainer = Color(0xFFFFF3B0),
    onTertiaryContainer = Color(0xFF322800),
    background = Color(0xFFFFF9FD),
    onBackground = Color(0xFF211A20),
    surface = Color(0xFFFFF9FD),
    onSurface = Color(0xFF211A20),
    surfaceVariant = Color(0xFFF6E7F1),
    onSurfaceVariant = Color(0xFF54434F),
    outline = Color(0xFF87727F),
)

private val DarkColors = darkColorScheme(
    primary = MagentaLight,
    onPrimary = Color(0xFF5B0040),
    primaryContainer = Color(0xFF7D0F5A),
    onPrimaryContainer = Color(0xFFFFDDF2),
    secondary = CyanLight,
    onSecondary = Color(0xFF003545),
    secondaryContainer = Color(0xFF004D63),
    onSecondaryContainer = Color(0xFFD4F4FF),
    tertiary = LemonLight,
    tertiaryContainer = Color(0xFF574800),
    onTertiaryContainer = Color(0xFFFFF3B0),
    background = Color(0xFF17121A),
    onBackground = Color(0xFFEDE0E8),
    surface = Color(0xFF17121A),
    onSurface = Color(0xFFEDE0E8),
    surfaceVariant = Color(0xFF3A2F38),
    onSurfaceVariant = Color(0xFFD7C1D0),
    outline = Color(0xFF9F8C99),
)

@Composable
fun SakiTheme(content: @Composable () -> Unit) {
    MaterialTheme(
        colorScheme = if (isSystemInDarkTheme()) DarkColors else LightColors,
        typography = Typography(),
        content = content,
    )
}
