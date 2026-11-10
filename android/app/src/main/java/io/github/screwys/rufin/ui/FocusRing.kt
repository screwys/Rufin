package io.github.screwys.rufin.ui

import androidx.compose.foundation.border
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.composed
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.graphics.Shape
import androidx.compose.ui.input.InputMode
import androidx.compose.ui.platform.LocalInputModeManager
import androidx.compose.ui.unit.dp

/** A ring around what a D-pad or a keyboard has focused; what holds the focus while touch drives the app
 *  (a seekbar, a field) shows none. Goes before the click. */
internal fun Modifier.focusRing(shape: Shape): Modifier = composed {
    var focused by remember { mutableStateOf(false) }
    val color = MaterialTheme.colorScheme.onSurface
    val keys = LocalInputModeManager.current.inputMode == InputMode.Keyboard
    onFocusChanged { focused = it.isFocused }.then(if (focused && keys) Modifier.border(3.dp, color, shape) else Modifier)
}
