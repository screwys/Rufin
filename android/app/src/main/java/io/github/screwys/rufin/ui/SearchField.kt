package io.github.screwys.rufin.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.core.translate

/** A filled search field: the lens, the text over its placeholder, and Clear once there is text. */
@Composable
internal fun SearchField(value: String, onValueChange: (String) -> Unit, modifier: Modifier = Modifier,
    placeholder: String = translate("Search"), keyboardOptions: KeyboardOptions = KeyboardOptions.Default) {
    BasicTextField(value, onValueChange, modifier, singleLine = true, keyboardOptions = keyboardOptions,
        textStyle = MaterialTheme.typography.bodyLarge.copy(color = MaterialTheme.colorScheme.onSurface),
        cursorBrush = SolidColor(MaterialTheme.colorScheme.primary), decorationBox = { input ->
            Row(Modifier.height(44.dp).clip(RoundedCornerShape(22.dp)).background(MaterialTheme.colorScheme.surfaceContainerHigh)
                .padding(horizontal = 14.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                RufinIcon("rufin-search-symbolic", null, Modifier.size(20.dp), MaterialTheme.colorScheme.onSurfaceVariant)
                Box(Modifier.weight(1f)) {
                    if (value.isEmpty()) Text(placeholder, style = MaterialTheme.typography.bodyLarge,
                        color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1)
                    input()
                }
                if (value.isNotEmpty()) RufinIcon("rufin-window-close-symbolic", translate("Clear"),
                    Modifier.size(18.dp).clip(CircleShape).clickable { onValueChange("") })
            }
        })
}
