package io.github.screwys.rufin.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import io.github.screwys.rufin.core.AndroidException
import io.github.screwys.rufin.shared.rufin_core.SettingsException
import io.github.screwys.rufin.shared.rufin_core.SourceAuthorizationException
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp

@Composable
internal fun ErrorRow(message: String, actionText: String, action: () -> Unit) {
    Row(Modifier.fillMaxWidth().padding(start = 16.dp, end = 8.dp, top = 4.dp, bottom = 4.dp), verticalAlignment = Alignment.CenterVertically) {
        ErrorNotice(message, Modifier.weight(1f))
        TextButton(onClick = action) { Text(actionText) }
    }
}

@Composable
internal fun ErrorRow(failure: Throwable, actionText: String, action: () -> Unit) = ErrorRow(failure.userMessage(), actionText, action)

internal fun Throwable.userMessage(): String = when (this) {
    is AndroidException.Failure -> reason
    is SettingsException.Message -> reason
    is SourceAuthorizationException.Failed -> reason
    else -> message ?: toString()
}

@Composable
internal fun ErrorNotice(message: String, modifier: Modifier = Modifier) {
    Text(message, modifier, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodyMedium)
}

@Composable
internal fun ErrorNotice(failure: Throwable) = ErrorNotice(failure.userMessage())
