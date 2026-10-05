package io.github.screwys.rufin.ui

import androidx.compose.foundation.pager.PagerState
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.TabIndicatorScope
import androidx.compose.material3.TabRowDefaults
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun TabIndicatorScope.PagerTabIndicator(pager: PagerState) {
    TabRowDefaults.PrimaryIndicator(Modifier.tabIndicatorLayout { measurable, constraints, tabs ->
        if (tabs.isEmpty()) layout(0, 0) {} else {
            val position = (pager.currentPage + pager.currentPageOffsetFraction).coerceIn(0f, tabs.lastIndex.toFloat())
            val from = position.toInt()
            val to = (from + 1).coerceAtMost(tabs.lastIndex)
            val fraction = position - from
            val start = tabs[from].left.value + (tabs[from].width.value - tabs[from].contentWidth.value) / 2
            val end = tabs[to].left.value + (tabs[to].width.value - tabs[to].contentWidth.value) / 2
            val left = (start + (end - start) * fraction).dp.roundToPx()
            val width = (tabs[from].contentWidth.value + (tabs[to].contentWidth.value - tabs[from].contentWidth.value) * fraction).dp.roundToPx()
            val placed = measurable.measure(constraints.copy(minWidth = width, maxWidth = width))
            layout(constraints.maxWidth, placed.height) { placed.placeRelative(left, 0) }
        }
    }, height = 3.dp, width = Dp.Unspecified)
}
