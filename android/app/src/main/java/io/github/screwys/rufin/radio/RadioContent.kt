package io.github.screwys.rufin.radio

import androidx.activity.compose.BackHandler
import androidx.compose.animation.core.animateOffsetAsState
import androidx.compose.animation.core.snap
import androidx.compose.animation.core.spring
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.zIndex
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.media3.common.util.UnstableApi
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.browse.BrowseEmptyState
import io.github.screwys.rufin.browse.ButtonRow
import io.github.screwys.rufin.browse.FactsPill
import io.github.screwys.rufin.browse.MetaLine
import io.github.screwys.rufin.browse.OnBand
import io.github.screwys.rufin.browse.ShowcasePill
import io.github.screwys.rufin.browse.ShowcaseRound
import io.github.screwys.rufin.browse.showcaseBand
import io.github.screwys.rufin.core.AndroidRadio
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.player.LiveMark
import io.github.screwys.rufin.player.PlayerConnection
import io.github.screwys.rufin.shared.library.SidebarPin
import io.github.screwys.rufin.shared.rufin_core.HeardSong
import io.github.screwys.rufin.shared.rufin_core.OnAir
import io.github.screwys.rufin.shared.rufin_core.RadioPage
import io.github.screwys.rufin.shared.rufin_core.Refresh
import io.github.screwys.rufin.shared.rufin_core.Station
import io.github.screwys.rufin.ui.Artwork
import io.github.screwys.rufin.ui.CoverShape
import io.github.screwys.rufin.ui.ErrorRow
import io.github.screwys.rufin.ui.LocalPlayerBottomPadding
import io.github.screwys.rufin.ui.PageLoading
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.ui.focusRing
import io.github.screwys.rufin.ui.openMediaUri
import io.github.screwys.rufin.ui.rememberArtwork
import io.github.screwys.rufin.ui.rufinMarquee
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.flow.drop
import kotlinx.coroutines.launch

/** What the Radio page and the bar over it share: the page as core answers it, the Add sheet the bar's
 *  button opens, and Select, in which the added stations are edited, deleted and reordered. */
internal class RadioState {
    var page by mutableStateOf<RadioPage?>(null)
    var adding by mutableStateOf(false)
    var selecting by mutableStateOf(false)
}

/** The Radio page, the tuner: the station on air, the selected source's stations, then the ones Rufin
 *  suggests. `filter` is the bar's search, which narrows the cards of both sections by name. */
@androidx.annotation.OptIn(UnstableApi::class)
@Composable
internal fun RadioContent(model: RufinConnection, player: PlayerConnection, state: RadioState, filter: String,
    onClearFilter: () -> Unit) {
    var bridge by remember { mutableStateOf<AndroidRadio?>(null) }
    var failure by remember { mutableStateOf<Throwable?>(null) }
    val reads = remember { Channel<Unit>(Channel.CONFLATED) }
    var editing by remember { mutableStateOf<Station?>(null) }
    var deleting by remember { mutableStateOf<Station?>(null) }
    val service = model.service
    LaunchedEffect(service, model.ready) {
        if (!model.ready) return@LaunchedEffect
        val radio = service?.runtime?.value?.getOrNull()?.radio() ?: return@LaunchedEffect
        bridge = radio
        try {
            coroutineScope {
                // Opening reads the server's stations too; the stored ones show meanwhile, and list offline.
                var open = true
                reads.trySend(Unit)
                launch {
                    model.changes.collect { changed ->
                        val refresh = radio.refresh(changed)
                        if (refresh == Refresh.REOPEN) open = true
                        if (refresh != Refresh.NOTHING) reads.trySend(Unit)
                    }
                }
                // The cards show which stations are pinned.
                launch { snapshotFlow { model.preferences?.pins }.drop(1).collect { reads.trySend(Unit) } }
                // The on-air header, the heard songs and the playing marks follow playback.
                launch {
                    snapshotFlow {
                        listOf(player.currentRow, player.currentId?.mediaUri, player.playback?.transport?.desiredPlaying, player.sourceLabels)
                    }.drop(1).collect { reads.trySend(Unit) }
                }
                for (read in reads) {
                    if (open) {
                        open = false
                        launch {
                            try { radio.readServer() }
                            catch (cancelled: CancellationException) { throw cancelled }
                            catch (_: Exception) {}
                        }
                    }
                    try {
                        state.page = radio.page()
                        failure = null
                    } catch (cancelled: CancellationException) { throw cancelled }
                    catch (error: Exception) { failure = error }
                }
            }
        } finally {
            bridge = null
            radio.destroy()
        }
    }
    DisposableEffect(state) { onDispose { state.selecting = false } }
    if (state.selecting) BackHandler { state.selecting = false }
    val page = state.page
    val radio = bridge
    if (page == null) {
        failure?.let { ErrorRow(it, translate("Retry")) { reads.trySend(Unit) } } ?: PageLoading()
        return
    }
    fun play(station: Station, channel: String? = null) = model.runAction {
        model.connectedService().playRequest {
            radio?.play(station.key, station.listed.takeIf { station.key == null }, channel)
        }
    }
    val query = filter.trim()
    val stations = remember(page.stations, query) { page.stations.filter { it.name.contains(query, ignoreCase = true) } }
    val suggested = remember(page.suggested, query) { page.suggested.filter { it.name.contains(query, ignoreCase = true) } }
    val context = LocalContext.current
    BoxWithConstraints(Modifier.fillMaxSize()) {
        val edge = if (maxWidth < 600.dp) 12.dp else 16.dp
        val gap = if (maxWidth < 600.dp) 10.dp else 12.dp
        // Cards of at least 280 dp: one column on a phone, a grid on a foldable and a tablet.
        val columns = ((maxWidth - edge * 2 + gap) / (280.dp + gap)).toInt().coerceAtLeast(1)
        // A slim card keeps its room for the text.
        val logo = if ((maxWidth - edge * 2 - gap * (columns - 1)) / columns >= 340.dp) 72.dp else 56.dp
        // The card dragged in Select: its place among the added stations, the place it is over, and how far
        // apart two places are.
        var dragFrom by remember { mutableIntStateOf(-1) }
        var dragTo by remember { mutableIntStateOf(-1) }
        var dragStep by remember { mutableStateOf(Offset.Zero) }
        val cards: @Composable (List<Station>) -> Unit = { row ->
            // Every card of a row is as tall as its tallest. The dragged card's row is over the others.
            Row(Modifier.zIndex(if (dragFrom >= 0 && stations.getOrNull(dragFrom) in row) 1f else 0f).fillMaxWidth().padding(horizontal = edge)
                .height(IntrinsicSize.Max), horizontalArrangement = Arrangement.spacedBy(gap)) {
                row.forEach { station ->
                    val place = if (station.key != null) stations.indexOf(station) else -1
                    // While one is dragged, the cards between its place and the one it is over stand a place nearer to where it came from.
                    val stand = when {
                        dragFrom < 0 || place < 0 -> place
                        place in dragFrom + 1..dragTo -> place - 1
                        place in dragTo until dragFrom -> place + 1
                        else -> place
                    }
                    StationCard(station, player, page.savedOn, logo, Modifier.weight(1f).fillMaxHeight(), state.selecting && station.key != null, gap,
                        Offset((stand % columns - place % columns) * dragStep.x, (stand / columns - place / columns) * dragStep.y), dragFrom >= 0,
                        onSelect = { state.selecting = true },
                        onDrag = { columnsMoved, rowsMoved, step ->
                            dragFrom = place
                            dragTo = (place + rowsMoved * columns + columnsMoved).coerceIn(0, stations.lastIndex)
                            dragStep = step
                        },
                        // The card goes to the place of the one it was dropped on, among all the added stations; the
                        // page shows the order at once and core keeps it.
                        onDrop = { moved ->
                            val target = stations.getOrNull(dragTo)
                            val to = page.stations.indexOf(target)
                            if (moved && target != null && target != station && to >= 0) {
                                val order = (page.stations - station).toMutableList().also { it.add(to.coerceAtMost(it.size), station) }
                                state.page = page.copy(stations = order)
                                model.runAction {
                                    radio?.reorder(order.mapNotNull { it.key })
                                    reads.trySend(Unit)
                                }
                            }
                            dragFrom = -1
                            dragTo = -1
                        },
                        onPlay = { channel -> play(station, channel) },
                        onPin = { pin, pinned -> model.browse.pin(pin, pinned) }, onEdit = { editing = station },
                        // A station of Rufin's list comes back from Suggested; any other is asked about first.
                        onDelete = { if (station.listed != null) model.runAction { station.key?.let { radio?.delete(it) } } else deleting = station },
                        onAdd = { model.runAction { station.listed?.let { radio?.addListed(it) } } })
                }
                repeat(columns - row.size) { Spacer(Modifier.weight(1f)) }
            }
        }
        LazyColumn(Modifier.fillMaxSize(), verticalArrangement = Arrangement.spacedBy(gap),
            contentPadding = PaddingValues(bottom = 12.dp + LocalPlayerBottomPadding.current)) {
            // The station that plays, or the page's first one ready to play: always there, so a station
            // going on air moves nothing under it.
            page.onAir?.let { onAir ->
                item(key = "onAir") {
                    OnAirHeader(onAir, player, Modifier.padding(start = edge, end = edge, top = gap),
                        onToggle = { if (onAir.tuned) player.togglePlayback() else play(onAir.station) },
                        onHomepage = { model.runAction { openMediaUri(context, onAir.station.homepage) } },
                        onPin = { pin, pinned -> model.browse.pin(pin, pinned) }, onEdit = { editing = onAir.station })
                }
            }
            if (query.isEmpty() || stations.isNotEmpty()) item(key = "stations") { SectionHeading("Stations", edge) }
            items(stations.chunked(columns), key = { "station:${it.first().key}" }) { cards(it) }
            if (suggested.isNotEmpty()) {
                item(key = "suggested") { SectionHeading("Suggested", edge) }
                items(suggested.chunked(columns), key = { "listed:${it.first().listed}" }) { cards(it) }
            }
            if (query.isNotEmpty() && stations.isEmpty() && suggested.isEmpty()) item(key = "nothing") {
                BrowseEmptyState(filtered = true, icon = "rufin-radio-station-symbolic", resetFilters = onClearFilter)
            }
        }
    }
    if (radio != null) {
        if (state.adding) AddStationSheet(model, player, radio, page.savedOn) { state.adding = false }
        editing?.let { station -> EditStationDialog(model, player, radio, station) { editing = null } }
        deleting?.let { station ->
            AlertDialog(onDismissRequest = { deleting = null }, title = { Text(translate("Delete Station")) },
                text = { Text(translate("Delete \"{name}\"?").replace("{name}", station.name)) },
                confirmButton = { TextButton({ deleting = null; model.runAction { station.key?.let { radio.delete(it) } } }) { Text(translate("Delete")) } },
                dismissButton = { TextButton({ deleting = null }) { Text(translate("Cancel")) } })
        }
    }
}

@Composable
private fun SectionHeading(title: String, edge: Dp) {
    Text(translate(title), Modifier.padding(start = edge + 4.dp, end = edge, top = 6.dp), style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
}

/** A station's picture: a rounded square. One without shows the radio icon on a tile tinted apart from the
 *  card or row under it. */
@Composable
internal fun StationLogo(binding: ByteArray?, player: PlayerConnection, size: Dp) {
    val cover = rememberArtwork(binding, player, with(LocalDensity.current) { size.roundToPx() })
    if (cover.image == null && cover.missing) Box(Modifier.size(size).clip(CoverShape)
        .background(MaterialTheme.colorScheme.onSurface.copy(alpha = .11f)),
        contentAlignment = Alignment.Center) {
        RufinIcon("rufin-radio-station-symbolic", null, Modifier.size(size / 2), MaterialTheme.colorScheme.onSurfaceVariant)
    } else Artwork(cover, Modifier.size(size).clip(CoverShape))
}

/** A station's genres under its name: the tag icon, then the genres on one line. */
@Composable
internal fun StationGenres(genres: List<String>) {
    if (genres.isEmpty()) return
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
        RufinIcon("rufin-tag-outline-symbolic", null, Modifier.size(13.dp), MaterialTheme.colorScheme.onSurfaceVariant)
        Text(genres.joinToString(", "), maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

/** The fill of a card's button and chips: a tint of the card's text, as the showcase's tonal buttons, so
 *  they stand apart from the card in the light theme as in the dark one. */
private val cardControl @Composable get() = MaterialTheme.colorScheme.onSurface.copy(alpha = .11f)

/** A round filled button at the end of a card; `active` draws the icon of a toggle that is on in the accent. */
@Composable
private fun CardButton(icon: String, label: String, active: Boolean = false, onClick: () -> Unit) {
    Surface(onClick, Modifier.size(40.dp).focusRing(CircleShape), shape = CircleShape, color = cardControl,
        contentColor = if (active) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurface) {
        Box(contentAlignment = Alignment.Center) { RufinIcon(icon, label, Modifier.size(20.dp)) }
    }
}

/** A station's pin, which `button` draws with its label and whether it is on. The pin of a station with
 *  channels lists them: each is saved by itself. */
@Composable
private fun StationPin(station: Station, pin: SidebarPin, onPin: (SidebarPin, Boolean) -> Unit,
    button: @Composable (label: String, active: Boolean, onClick: () -> Unit) -> Unit) {
    if (station.channels.isEmpty()) button(if (station.pinned) "Remove from Saved" else "Add to Saved", station.pinned) { onPin(pin, !station.pinned) }
    else Box {
        var saving by remember { mutableStateOf(false) }
        button("Add to Saved", station.pinned || station.channels.any { it.pinned }) { saving = true }
        DropdownMenu(saving, { saving = false }) {
            station.channels.forEach { channel ->
                DropdownMenuItem(text = { Text(channel.name) },
                    onClick = { saving = false; channel.pin?.let { onPin(it, !channel.pinned) } },
                    trailingIcon = if (channel.pinned) { { RufinIcon("rufin-object-select-symbolic", null, Modifier.size(18.dp)) } } else null)
            }
        }
    }
}

/** A station card: its picture, name, genres, description and channels. A tap plays the station, a
 *  channel's chip that channel; a suggested one ends with +. A long press on an added one opens Select,
 *  where it ends with its pin, Edit, Delete and the handle it is dragged to another's place by, as Saved's rows.
 *  `shift` is how far it stands from its place while another card is dragged past it (`dragging`). */
@OptIn(ExperimentalLayoutApi::class, ExperimentalFoundationApi::class)
@Composable
private fun StationCard(station: Station, player: PlayerConnection, savedOn: String?, logo: Dp, modifier: Modifier,
    selecting: Boolean, gap: Dp, shift: Offset, dragging: Boolean, onSelect: () -> Unit,
    onDrag: (Int, Int, Offset) -> Unit, onDrop: (Boolean) -> Unit,
    onPlay: (String?) -> Unit, onPin: (SidebarPin, Boolean) -> Unit, onEdit: () -> Unit, onDelete: () -> Unit, onAdd: () -> Unit) {
    val scheme = MaterialTheme.colorScheme
    var drag by remember { mutableStateOf(Offset.Zero) }
    var size by remember { mutableStateOf(IntSize.Zero) }
    val step = with(LocalDensity.current) { gap.toPx() }
    val dragged by rememberUpdatedState(onDrag)
    val dropped by rememberUpdatedState(onDrop)
    // The others slide to where they stand; the drop puts every card in its new place at once.
    val slide by animateOffsetAsState(shift, if (dragging) spring() else snap(), label = "stationSlide")
    Surface(modifier.zIndex(if (drag == Offset.Zero) 0f else 1f).graphicsLayer { translationX = drag.x + slide.x; translationY = drag.y + slide.y }
        .onSizeChanged { size = it }.focusRing(RoundedCornerShape(18.dp)).clip(RoundedCornerShape(18.dp))
        .combinedClickable(onClick = { onPlay(null) }, onLongClick = onSelect.takeIf { station.key != null }),
        shape = RoundedCornerShape(18.dp), color = scheme.surfaceContainerHigh, contentColor = scheme.onSurface) {
        // The button sits nearer the card's corner than the picture, so the name beside it stays level with the picture's top.
        Row(Modifier.padding(start = 12.dp, top = 6.dp, end = 8.dp, bottom = 12.dp), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            Box(Modifier.padding(top = 6.dp)) { StationLogo(station.artworkBinding, player, logo) }
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                // The name's row ends with the card's button; the lines under it reach the card's edge.
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(station.name, Modifier.weight(1f), maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.titleMedium,
                        fontWeight = FontWeight.SemiBold, color = if (station.playing) scheme.primary else scheme.onSurface)
                    if (selecting) {
                        // Without Saved a station has no pin.
                        station.pin?.let { pin ->
                            StationPin(station, pin, onPin) { label, active, click -> CardButton(if (active) "rufin-bookmark-filled-symbolic" else "rufin-bookmark-symbolic", translate(label), active, click) }
                        }
                        CardButton("rufin-document-edit-symbolic", translate("Edit"), onClick = onEdit)
                        CardButton("rufin-user-trash-symbolic", translate("Delete"), onClick = onDelete)
                        Box(Modifier.size(40.dp).pointerInput(station.key) {
                            detectDragGestures(onDragEnd = { dropped(true); drag = Offset.Zero },
                                onDragCancel = { dropped(false); drag = Offset.Zero }) { change, amount ->
                                change.consume()
                                drag += amount
                                if (size.width > 0 && size.height > 0) dragged(Math.round(drag.x / (size.width + step)),
                                    Math.round(drag.y / (size.height + step)), Offset(size.width + step, size.height + step))
                            }
                        }, contentAlignment = Alignment.Center) {
                            RufinIcon("rufin-list-drag-handle-symbolic", translate("Drag to reorder"), Modifier.size(20.dp), scheme.onSurfaceVariant)
                        }
                    // Its room stays, so the name's row is as tall on every card.
                    } else if (station.key != null) Spacer(Modifier.height(40.dp))
                    else CardButton("rufin-list-add-symbolic",
                        savedOn?.let { translate("Add to {saved_on}").replace("{saved_on}", it) } ?: translate("Add"), onClick = onAdd)
                }
                Column(Modifier.padding(end = 4.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    StationGenres(station.genres)
                    if (station.description.isNotBlank()) Text(station.description, maxLines = 2, overflow = TextOverflow.Ellipsis,
                        style = MaterialTheme.typography.bodyMedium, color = scheme.onSurface.copy(alpha = .86f))
                    if (station.channels.isNotEmpty()) FlowRow(Modifier.padding(top = 4.dp), horizontalArrangement = Arrangement.spacedBy(6.dp),
                        verticalArrangement = Arrangement.spacedBy(6.dp)) {
                        station.channels.forEach { channel ->
                            Surface({ onPlay(channel.name) }, Modifier.height(30.dp).focusRing(RoundedCornerShape(15.dp)), shape = RoundedCornerShape(15.dp),
                                color = if (channel.playing) scheme.primary else cardControl,
                                contentColor = if (channel.playing) scheme.onPrimary else scheme.onSurface) {
                                Box(Modifier.padding(horizontal = 13.dp), contentAlignment = Alignment.Center) {
                                    Text(channel.name, maxLines = 1, style = MaterialTheme.typography.labelLarge, fontWeight = FontWeight.SemiBold)
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/** The station on air, one card in its band's colours: its picture and, ending level with it, the station
 *  with its channel, the announced song and its artist, one line of Live, the format and the genres, then
 *  the buttons: Play, and an added station's pin and Edit, as its card has in Select. The songs heard before
 *  sit under those, or beside them in a wide card. */
@Composable
private fun OnAirHeader(onAir: OnAir, player: PlayerConnection, modifier: Modifier, onToggle: () -> Unit, onHomepage: () -> Unit,
    onPin: (SidebarPin, Boolean) -> Unit, onEdit: () -> Unit) {
    BoxWithConstraints(modifier.fillMaxWidth()) {
        val wide = maxWidth >= 700.dp
        val picture = if (wide) 164.dp else (maxWidth * .34f).coerceIn(120.dp, 150.dp)
        val station = onAir.station
        val stops = rememberArtwork(station.artworkBinding, player, with(LocalDensity.current) { picture.roundToPx() }).image?.band
        // The song announced now is among the names; the songs heard are the ones before it.
        val now = onAir.heard.firstOrNull()?.let { it.title == onAir.title && it.artist == onAir.artist.orEmpty() } == true
        val earlier = if (now) onAir.heard.drop(1) else onAir.heard
        val top: @Composable (Modifier) -> Unit = { place ->
            // The picture stays at the top; the text ends level with it, or grows the card past it.
            Row(place, horizontalArrangement = Arrangement.spacedBy(if (wide) 24.dp else 16.dp)) {
                Box(Modifier.size(picture).shadow(14.dp, CoverShape, ambientColor = Color.Black.copy(alpha = .4f), spotColor = Color.Black.copy(alpha = .4f))
                    .clip(CoverShape).background(MaterialTheme.colorScheme.surfaceContainerHigh)) {
                    StationLogo(station.artworkBinding, player, picture)
                }
                Column(Modifier.weight(1f).heightIn(min = picture), verticalArrangement = Arrangement.Bottom) {
                    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        Text(station.name, Modifier.weight(1f, fill = false).rufinMarquee(), maxLines = 1,
                            style = if (wide) MaterialTheme.typography.headlineMedium else MaterialTheme.typography.titleLarge,
                            fontWeight = FontWeight.ExtraBold)
                        onAir.channel?.let { channel ->
                            Box(Modifier.height(24.dp).clip(RoundedCornerShape(12.dp)).background(OnBand.copy(alpha = .16f)).padding(horizontal = 10.dp),
                                contentAlignment = Alignment.Center) {
                                Text(channel, maxLines = 1, style = MaterialTheme.typography.labelMedium, fontWeight = FontWeight.SemiBold)
                            }
                        }
                    }
                    onAir.title?.takeIf(String::isNotBlank)?.let { title ->
                        Text(title, Modifier.padding(top = 2.dp), maxLines = 1, overflow = TextOverflow.Ellipsis,
                            style = MaterialTheme.typography.bodyLarge, fontWeight = FontWeight.SemiBold)
                    }
                    onAir.artist?.takeIf(String::isNotBlank)?.let { artist ->
                        Text(artist, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodyMedium,
                            color = OnBand.copy(alpha = .84f))
                    }
                    // Live and the format in the facts' pill, then the genres.
                    MetaLine(station.genres, true, Modifier.padding(top = if (wide) 12.dp else 8.dp)) {
                        FactsPill(listOfNotNull(onAir.format?.takeIf(String::isNotBlank)?.let { Triple("rufin-sound-symbolic", "Format", it) })) {
                            LiveMark(MaterialTheme.typography.labelLarge)
                        }
                    }
                    Box(Modifier.padding(top = 12.dp)) {
                        ButtonRow { labels ->
                            if (onAir.playing) ShowcasePill("rufin-media-playback-pause-symbolic", "Pause", true, onToggle, labelled = labels == 2)
                            else ShowcasePill("rufin-media-playback-start-symbolic", "Play", true, onToggle, labelled = labels == 2)
                            if (station.key != null) {
                                station.pin?.let { pin ->
                                    StationPin(station, pin, onPin) { label, active, click ->
                                        ShowcaseRound(if (active) "rufin-bookmark-filled-symbolic" else "rufin-bookmark-symbolic", label, click, OnBand, onBand = true)
                                    }
                                }
                                ShowcaseRound("rufin-document-edit-symbolic", "Edit", onEdit, onBand = true)
                            }
                            if (station.homepage.isNotBlank()) ShowcaseRound("rufin-external-link-symbolic", "Open homepage", onHomepage, onBand = true)
                        }
                    }
                }
            }
        }
        val card = Modifier.fillMaxWidth().clip(RoundedCornerShape(8.dp)).then(showcaseBand(stops)).padding(if (wide) 20.dp else 16.dp)
        val line = Color.White.copy(alpha = .16f)
        CompositionLocalProvider(LocalContentColor provides OnBand) {
            if (wide) Row(card, verticalAlignment = Alignment.CenterVertically) {
                top(Modifier.weight(1f))
                if (earlier.isNotEmpty()) Heard(earlier, Modifier.padding(start = 24.dp).width(340.dp)
                    .drawBehind { drawLine(line, Offset.Zero, Offset(0f, size.height), 1.dp.toPx()) }.padding(start = 20.dp))
            } else Column(card) {
                top(Modifier.fillMaxWidth())
                if (earlier.isNotEmpty()) {
                    HorizontalDivider(Modifier.padding(top = 14.dp, bottom = 12.dp), color = line)
                    Heard(earlier, Modifier.fillMaxWidth())
                }
            }
        }
    }
}

/** The songs the station announced before the one that plays, newest first, a line each on the on-air
 *  card: the time it began, its title, then its artist. */
@Composable
private fun Heard(songs: List<HeardSong>, modifier: Modifier) {
    val clock = remember { SimpleDateFormat("HH:mm", Locale.getDefault()) }
    // A time's width at any font size, so the titles start in one line.
    val time = with(LocalDensity.current) { 44.sp.toDp() }
    val dim = OnBand.copy(alpha = .72f)
    Column(modifier, verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Text(translate("Heard").uppercase(), style = MaterialTheme.typography.labelMedium, fontWeight = FontWeight.Bold, color = dim)
        songs.forEach { song ->
            Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                Text(clock.format(Date(song.atUnixSeconds * 1000)), Modifier.width(time).alignByBaseline(), maxLines = 1,
                    style = MaterialTheme.typography.bodySmall, color = dim)
                Text(buildAnnotatedString {
                    withStyle(SpanStyle(fontWeight = FontWeight.Medium)) { append(song.title) }
                    if (song.artist.isNotBlank()) withStyle(SpanStyle(color = dim)) { append("  ${song.artist}") }
                }, Modifier.weight(1f).alignByBaseline(), maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodyMedium)
            }
        }
    }
}
