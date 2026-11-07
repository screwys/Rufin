package io.github.screwys.rufin.browse

import io.github.screwys.rufin.shared.rufin_core.Changed

import androidx.compose.runtime.*
import androidx.compose.runtime.snapshots.Snapshot
import io.github.screwys.rufin.core.*
import kotlinx.coroutines.*

/** The one holder of the rows a surface read: a page's, or Search's. Committed changes patch the
 * rows it holds through core, each row with the View that read it. */
internal open class ShownRows(private val browse: BrowseConnection, protected val scope: CoroutineScope) {
    /** How many favorite, rating and download changes were committed while the surface is open. */
    protected var downloads by mutableIntStateOf(0)
    /** Rows core patched, by identity: each patches only rows of the View that read the row it
     * patched, read before its change; a row read since shows itself. Written on the main thread
     * only; a patch goes once the surface stops holding its row. */
    private val patched = mutableStateMapOf<String, Patch>()
    private class Patch(val revision: Int, val view: AndroidView?, val row: AndroidBrowseRow)
    /** The rows the surface holds by identity, most recently read or shown last: those on screen
     * and those read lately, which scroll back in without a read. Committed changes patch them
     * through core. */
    private val held = object : LinkedHashMap<String, BrowseItem>(HELD_ROWS, 0.75f, true) {
        override fun removeEldestEntry(eldest: MutableMap.MutableEntry<String, BrowseItem>): Boolean = size > HELD_ROWS
    }
    /** The changes the rows the surface read with no View show, its own as each View's is. */
    private val viewless = AndroidShownChanges()
    /** A page's header row and View, which a change shows with the rows of that View. */
    protected open val header: AndroidBrowseRow? get() = null
    open val pageView: AndroidView? get() = null
    protected open fun showHeader(row: AndroidBrowseRow) {}

    /** `item` showing the favorites, ratings and downloads committed since the surface read it. */
    @Composable
    fun shown(item: BrowseItem): BrowseItem {
        val key = shownKey(item)
        DisposableEffect(key, item) {
            hold(listOf(item))
            onDispose {}
        }
        val row = patchOf(item)?.row ?: return item
        return remember(item, row) { item.copy(row = row) }
    }

    /** A row's identity among those the surface holds, at its place: a listen or entry differs
     * from another of the same track. */
    private fun shownKey(item: BrowseItem) = "${item.row.sourceId.orEmpty()}:${item.row.key}:${item.index}"

    /** Holds `items`, rows the surface read or shows, so committed changes patch them. A row read
     * later replaces one read earlier. */
    fun hold(items: List<BrowseItem>) = synchronized(held) {
        for (item in items) {
            val key = shownKey(item)
            val previous = held[key]
            if ((previous?.downloads ?: -1) <= item.downloads) held[key] = item
        }
    }

    /** The patch `item` shows: one of its View that came since it was read. */
    private fun patchOf(item: BrowseItem): Patch? =
        patched[shownKey(item)]?.takeIf { it.view === item.view && it.revision > item.downloads }

    /** The row `item` shows now: its patch when one came since it was read. */
    private fun current(item: BrowseItem): AndroidBrowseRow = patchOf(item)?.row ?: item.row

    /** Shows a committed change in the header and the rows the surface holds, through core:
     * favorites and ratings at once, and downloads once read, into each row as it shows then, so
     * an older change never undoes a newer one. Each row counts as the View that read it counts,
     * the header as the page. Runs on the main thread. */
    fun showChanges(changed: Changed) {
        val revision = ++downloads
        val holding = synchronized(held) { held.toMap() }
        patched.keys.retainAll(holding.keys)
        val items = holding.values.groupBy { it.view }
        val page = pageView
        showRead(changed, revision, page, header, items[page].orEmpty())
        for ((view, read) in items) if (view !== page) showRead(changed, revision, view, null, read)
    }

    private fun showRead(changed: Changed, revision: Int, view: AndroidView?, header: AndroidBrowseRow?, items: List<BrowseItem>) {
        val library = browse.library ?: return
        val offset = if (header != null) 1 else 0
        val showing = library.showChanges(listOfNotNull(header) + items.map(::current), changed, view, viewless)
        Snapshot.withMutableSnapshot {
            for (shown in showing.states) {
                val index = shown.index.toInt() - offset
                if (index < 0) showHeader(shown.row)
                else items[index].let { patched[shownKey(it)] = Patch(revision, it.view, shown.row) }
            }
        }
        val downloads = showing.downloads ?: return
        scope.launch(Dispatchers.Main) {
            val read = downloads.read()
            Snapshot.withMutableSnapshot {
                for (shown in read) {
                    val index = shown.index.toInt() - offset
                    if (index < 0) {
                        val current = this@ShownRows.header?.takeIf { pageView === view && it.key == header!!.key } ?: continue
                        library.takeDownloads(current, shown.row)?.let(::showHeader)
                        continue
                    }
                    val key = shownKey(items[index])
                    val item = synchronized(held) { held[key] }?.takeIf { it.view === items[index].view } ?: continue
                    val patch = patchOf(item)
                    val merged = library.takeDownloads(patch?.row ?: item.row, shown.row) ?: continue
                    patched[key] = Patch(maxOf(revision, patch?.revision ?: 0), item.view, merged)
                }
            }
        }
    }
}

/** How many rows a surface holds for patches: more than paging keeps and the screen shows. */
private const val HELD_ROWS = 512
