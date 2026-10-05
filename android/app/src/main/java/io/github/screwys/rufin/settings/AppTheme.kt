package io.github.screwys.rufin.settings

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.colorspace.ColorSpaces
import androidx.compose.ui.graphics.compositeOver
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.platform.LocalContext
import io.github.screwys.rufin.ui.LocalShowDownloadedBadges

internal val LocalTranslationRevision = staticCompositionLocalOf { 0 }
internal val LocalReduceMotion = staticCompositionLocalOf { false }

@Composable
internal fun AppTheme(preferences: PreferencesConnection, content: @Composable () -> Unit) {
    val configuration = LocalConfiguration.current
    val localeTags = configuration.locales.toLanguageTags()
    LaunchedEffect(localeTags) { preferences.refreshSystemLanguage() }
    val state = preferences.state
    val systemDark = isSystemInDarkTheme()
    val selected = state?.themes?.find { it.id == state.themeId }
    val dark = selected?.dark ?: when (state?.themeId) { "light" -> false; "dark" -> true; else -> systemDark }
    val theme = selected ?: state?.themes?.find { it.id == if (dark) "builtin:dark" else "builtin:light" }
    val systemAccent = selected == null && (state == null || state.accentId == "System")
    val context = LocalContext.current
    val defaults = remember(dark, systemAccent, context, configuration) {
        if (systemAccent && android.os.Build.VERSION.SDK_INT >= 31) {
        if (dark) dynamicDarkColorScheme(context) else dynamicLightColorScheme(context)
        } else if (dark) darkColorScheme() else lightColorScheme()
    }
    val colors = theme?.colors.orEmpty()
    val resolver = remember(colors) { ThemeColors(colors) }
    val scheme = remember(resolver, defaults, systemAccent) {
    val window = resolver.named("window-bg-color") ?: defaults.background
    val surface = resolver.named("view-bg-color") ?: window
    val foreground = resolver.named("view-fg-color") ?: defaults.onSurface
    val card = (resolver.named("card-bg-color") ?: defaults.surfaceContainer).compositeOver(surface)
    val accent = if (systemAccent) defaults.primary else resolver.named("accent-bg-color") ?: defaults.primary
    val accentContainer = accent.copy(alpha = .16f).compositeOver(surface)
    defaults.copy(
        primary = accent, onPrimary = if (systemAccent) defaults.onPrimary else resolver.named("accent-fg-color") ?: defaults.onPrimary,
        secondary = if (systemAccent) accent else resolver.named("accent-color") ?: accent,
        primaryContainer = accentContainer, onPrimaryContainer = foreground,
        secondaryContainer = accentContainer, onSecondaryContainer = foreground,
        onSurfaceVariant = foreground.copy(alpha = .78f),
        background = window, onBackground = resolver.named("window-fg-color") ?: foreground,
        surface = surface, onSurface = foreground,
        surfaceContainer = card, surfaceContainerLow = window,
        surfaceContainerHigh = resolver.named("dialog-bg-color") ?: card,
        surfaceContainerHighest = resolver.named("popover-bg-color") ?: card,
        error = resolver.named("error-color") ?: defaults.error,
        errorContainer = resolver.named("error-bg-color") ?: defaults.errorContainer,
        onErrorContainer = resolver.named("error-fg-color") ?: defaults.onErrorContainer,
        outline = foreground.copy(alpha = .42f).compositeOver(surface),
        outlineVariant = (resolver.named("sidebar-border-color") ?: foreground.copy(alpha = .18f)).compositeOver(surface),
    )
    }
    SideEffect { preferences.showThemeErrors(resolver.errors.toList()) }
    CompositionLocalProvider(LocalTranslationRevision provides preferences.translationRevision,
        LocalShowDownloadedBadges provides preferences.showDownloadedBadges,
        LocalReduceMotion provides preferences.reduceMotion) {
        MaterialTheme(colorScheme = scheme, content = content)
    }
}

/** Converts the shared theme's CSS color values into Compose colors. */
internal class ThemeColors(private val colors: Map<String, String>) {
    val errors = linkedSetOf<String>()
    fun named(name: String): Color? {
        val value = colors[name] ?: return null
        return resolve(value, setOf(name)).also {
            if (it == null) errors += "$name: $value"
        }
    }

    private fun resolve(value: String, seen: Set<String>): Color? {
        val text = value.trim()
        if (text.startsWith("var(")) {
            val name = text.removePrefix("var(").removeSuffix(")").trim().removePrefix("--")
            if (name in seen) return null
            return colors[name]?.let { resolve(it, seen + name) }
        }
        if (text.startsWith("rgb(") || text.startsWith("rgba(")) {
            val values = text.substringAfter('(').substringBeforeLast(')').trim().split(Regex("[ ,/]+"))
            if (values.size !in 3..4) return null
            fun channel(value: String) = value.removeSuffix("%").toFloatOrNull()?.let { if (value.endsWith('%')) it / 100f else it / 255f }
            val r = channel(values[0]) ?: return null
            val g = channel(values[1]) ?: return null
            val b = channel(values[2]) ?: return null
            val alpha = if (values.size == 4) {
                val token = values[3]
                val number = token.removeSuffix("%").toFloatOrNull() ?: return null
                if (token.endsWith('%')) number / 100f else number
            } else 1f
            return Color(r.coerceIn(0f, 1f), g.coerceIn(0f, 1f), b.coerceIn(0f, 1f), alpha.coerceIn(0f, 1f))
        }
        if (text.startsWith("oklab(from ")) {
            val reference = Regex("var\\(--([^)]*)\\)").find(text)?.groupValues?.get(1) ?: return null
            if (reference in seen) return null
            val source = colors[reference]?.let { resolve(it, seen + reference) }?.convert(ColorSpaces.Oklab) ?: return null
            val standalone = colors["standalone-color-oklab"] ?: return null
            val expression = Regex("(min|max)\\(l,\\s*([0-9.]+)\\)").find(standalone) ?: return null
            val limit = expression.groupValues[2].toFloatOrNull() ?: return null
            val lightness = if (expression.groupValues[1] == "min") minOf(source.red, limit) else maxOf(source.red, limit)
            return Color(lightness, source.green, source.blue, source.alpha, ColorSpaces.Oklab).convert(ColorSpaces.Srgb)
        }
        if (text.startsWith("color-mix(in srgb,")) {
            val arguments = splitArguments(text.removePrefix("color-mix(in srgb,").removeSuffix(")"))
            if (arguments.size != 2) return null
            fun part(value: String): Pair<Color, Float?>? {
                val match = Regex("\\s+([0-9.]+)%$").find(value)
                val color = resolve(if (match == null) value else value.substring(0, match.range.first), seen) ?: return null
                return color to match?.groupValues?.get(1)?.toFloatOrNull()?.div(100f)
            }
            val first = part(arguments[0]) ?: return null
            val second = part(arguments[1]) ?: return null
            val weight = first.second ?: (1f - (second.second ?: 0.5f))
            val other = second.second ?: (1f - weight)
            val sum = weight + other
            if (sum <= 0f) return null
            val alpha = (first.first.alpha * weight + second.first.alpha * other) / sum
            if (alpha == 0f) return Color.Transparent
            fun mix(a: Float, b: Float) = (a * first.first.alpha * weight + b * second.first.alpha * other) / sum / alpha
            return Color(mix(first.first.red, second.first.red), mix(first.first.green, second.first.green), mix(first.first.blue, second.first.blue), alpha * minOf(sum, 1f))
        }
        if (text == "transparent") return Color.Transparent
        if (text.startsWith('#') && text.length in listOf(4, 5, 7, 9)) {
            val digits = text.substring(1).let { if (it.length <= 4) it.flatMap { char -> listOf(char, char) }.joinToString("") else it }
            val rgb = digits.take(6).toLongOrNull(16) ?: return null
            val alpha = if (digits.length == 8) digits.takeLast(2).toIntOrNull(16) ?: return null else 255
            return Color(((alpha.toLong() shl 24) or rgb).toInt())
        }
        return runCatching { Color(android.graphics.Color.parseColor(text)) }.getOrNull()
    }

    private fun splitArguments(value: String): List<String> {
        var depth = 0
        var start = 0
        val values = mutableListOf<String>()
        value.forEachIndexed { index, char ->
            when (char) {
                '(' -> depth++
                ')' -> depth--
                ',' -> if (depth == 0) { values += value.substring(start, index).trim(); start = index + 1 }
            }
        }
        values += value.substring(start).trim()
        return values
    }
}
