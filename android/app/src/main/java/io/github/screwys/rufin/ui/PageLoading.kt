package io.github.screwys.rufin.ui

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import io.github.screwys.rufin.core.translate
import kotlinx.coroutines.delay

/** A page's place while it reads. The spinner shows only once the read is slow enough to notice, so a
 *  quick page draws once, with its content, instead of after a flash of the spinner. */
@Composable
internal fun PageLoading(modifier: Modifier = Modifier.fillMaxSize()) {
    var slow by remember { mutableStateOf(false) }
    LaunchedEffect(Unit) { delay(400); slow = true }
    Box(modifier, contentAlignment = Alignment.Center) {
        if (slow) CircularProgressIndicator(Modifier.semantics { contentDescription = translate("Loading...") })
    }
}
