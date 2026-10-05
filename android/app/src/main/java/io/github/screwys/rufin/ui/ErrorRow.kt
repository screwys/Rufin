package io.github.screwys.rufin.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import io.github.screwys.rufin.core.AndroidException
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp

@Composable
internal fun ErrorRow(message: String, actionText: String, action: () -> Unit) {
    ErrorNotice(message)
    Row(Modifier.fillMaxWidth().padding(start = 16.dp, end = 8.dp, top = 4.dp, bottom = 4.dp), verticalAlignment = Alignment.CenterVertically) {
        Spacer(Modifier.weight(1f))
        TextButton(onClick = action) { Text(actionText) }
    }
}

@Composable
internal fun ErrorRow(failure: Throwable, actionText: String, action: () -> Unit) = ErrorRow(failure.userMessage(), actionText, action)

internal val LocalFeedback = staticCompositionLocalOf<(String) -> Unit> { error("Feedback presenter is unavailable") }

internal fun Throwable.userMessage(): String = when (this) {
    is AndroidException.Failure -> reason
    else -> message ?: toString()
}

@Composable
internal fun ErrorNotice(message: String) {
    val feedback = LocalFeedback.current
    LaunchedEffect(message) { feedback(message) }
}

@Composable
internal fun ErrorNotice(failure: Throwable) = ErrorNotice(failure.userMessage())
