package io.github.screwys.rufin.ui

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.basicMarquee
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import io.github.screwys.rufin.settings.LocalReduceMotion

@OptIn(ExperimentalFoundationApi::class)
@Composable
internal fun Modifier.rufinMarquee(): Modifier = if (LocalReduceMotion.current) this else
    basicMarquee(iterations = Int.MAX_VALUE, initialDelayMillis = 2_000, repeatDelayMillis = 2_000)
