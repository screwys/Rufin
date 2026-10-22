package io.github.screwys.rufin.ui

import android.content.ClipData
import android.content.ClipDescription
import android.content.ClipboardManager
import android.content.Context
import android.os.Build
import android.os.PersistableBundle
import androidx.compose.material3.IconButton
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import io.github.screwys.rufin.core.translate
import kotlinx.coroutines.delay

@Composable
internal fun CopyIconButton(
    label: String,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
    sensitive: Boolean = false,
    clipboardLabel: String? = null,
    onCopy: ((String) -> Unit) -> Unit,
) {
    val context = LocalContext.current
    var copied by remember { mutableIntStateOf(0) }
    LaunchedEffect(copied) { if (copied > 0) { delay(1500); copied = 0 } }
    IconButton({
        onCopy { text ->
            val clip = ClipData.newPlainText(clipboardLabel ?: translate(label), text)
            if (sensitive && Build.VERSION.SDK_INT >= 33) {
                clip.description.extras = PersistableBundle().apply { putBoolean(ClipDescription.EXTRA_IS_SENSITIVE, true) }
            }
            (context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager).setPrimaryClip(clip)
            copied++
        }
    }, modifier, enabled = enabled) {
        RufinIcon(if (copied > 0) "rufin-object-select-symbolic" else "rufin-edit-copy-symbolic", translate(label))
    }
}
