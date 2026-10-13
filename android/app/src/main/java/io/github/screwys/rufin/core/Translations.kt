package io.github.screwys.rufin.core

@Volatile
private var messages: Map<String, String> = emptyMap()

internal fun installTranslations(translations: Map<String, String>) {
    messages = translations
}

internal fun translate(message: String): String = messages[message] ?: message
