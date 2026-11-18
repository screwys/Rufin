package io.github.screwys.rufin.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.unit.dp

/** Rufin's toast: Material's snackbar in the accent's tint, full width up to 480dp, with the action at its end and
 *  work in progress along its bottom edge (`progress` null while the total is unknown). */
@Composable
internal fun Toast(message: String, modifier: Modifier = Modifier, action: String? = null, onAction: () -> Unit = {},
    working: Boolean = false, progress: Float? = null) {
    Box(modifier.widthIn(max = 480.dp).fillMaxWidth().clip(SnackbarDefaults.shape)) {
        val scheme = MaterialTheme.colorScheme
        Snackbar(action = action?.let { label -> {
            TextButton(onAction, colors = ButtonDefaults.textButtonColors(contentColor = scheme.primary)) { Text(label) }
        } }, containerColor = scheme.primaryContainer, contentColor = scheme.onPrimaryContainer) {
            Text(message)
        }
        if (working) {
            val color = scheme.primary
            val track = scheme.primary.copy(alpha = .24f)
            val edge = Modifier.align(Alignment.BottomCenter).fillMaxWidth()
            if (progress != null) LinearProgressIndicator({ progress }, edge, color, track, StrokeCap.Butt, gapSize = 0.dp, drawStopIndicator = {})
            else LinearProgressIndicator(edge, color, track, StrokeCap.Butt, gapSize = 0.dp)
        }
    }
}
