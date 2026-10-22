package io.github.screwys.rufin.browse

import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.LinkAnnotation
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextLinkStyles
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.withLink
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.metadata.MetadataEditorScreen
import io.github.screwys.rufin.player.PlayerConnection
import io.github.screwys.rufin.settings.SettingsGroup
import io.github.screwys.rufin.shared.rufin_core.MediaDetails
import io.github.screwys.rufin.shared.rufin_core.MediaDetailsItem
import io.github.screwys.rufin.shared.rufin_core.Route
import io.github.screwys.rufin.ui.Artwork
import io.github.screwys.rufin.ui.CopyIconButton
import io.github.screwys.rufin.ui.DetailLinkIcon
import io.github.screwys.rufin.ui.ErrorNotice
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.ui.rememberArtwork
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun MediaDetailsDialog(
    model: RufinConnection, browse: BrowseConnection, item: BrowseItem,
    player: PlayerConnection, onDismiss: () -> Unit,
) {
    val context = LocalContext.current
    val initial = remember(item.row.mediaUri, item.row.kind) {
        when (item.row.kind) {
            "album", "album_header" -> MediaDetailsItem.Album(item.row.mediaUri)
            "artist" -> MediaDetailsItem.Artist(item.row.mediaUri, item.row.detailRoute is Route.AlbumArtistDetail)
            else -> MediaDetailsItem.Track(item.row.mediaUri)
        }
    }
    var target by remember(initial) { mutableStateOf(initial) }
    var history by remember(initial) { mutableStateOf(emptyList<MediaDetailsItem>()) }
    var revision by remember { mutableIntStateOf(0) }
    var editing by remember { mutableStateOf(false) }
    var data by remember(target, revision) { mutableStateOf<MediaDetails?>(null) }
    var failure by remember(target, revision) { mutableStateOf<Throwable?>(null) }
    LaunchedEffect(target, revision, browse.library) {
        try { data = withContext(Dispatchers.IO) { browse.library?.mediaDetails(target) } }
        catch (cancelled: CancellationException) { throw cancelled }
        catch (error: Exception) { failure = error }
    }
    val kind = when (target) {
        is MediaDetailsItem.Track -> "track"
        is MediaDetailsItem.Album -> "album"
        is MediaDetailsItem.Artist -> "artist"
    }
    val mediaUri = when (val id = target) {
        is MediaDetailsItem.Track -> id.v1
        is MediaDetailsItem.Album -> id.v1
        is MediaDetailsItem.Artist -> id.mediaUri
    }
    val heading = data?.heading ?: translate(when (kind) {
        "album" -> "Album details"; "artist" -> "Artist details"; else -> "Track details"
    })
    fun back() {
        if (history.isEmpty()) onDismiss()
        else { target = history.last(); history = history.dropLast(1) }
    }
    Dialog(onDismissRequest = ::back,
        properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false)) {
        Surface(Modifier.fillMaxSize()) {
            Scaffold(topBar = {
                TopAppBar(title = { Text(heading) }, navigationIcon = {
                    IconButton(::back) { RufinIcon("rufin-go-previous-symbolic", translate("Back")) }
                }, actions = {
                    if (data?.canEdit == true) IconButton({ editing = true }) {
                        RufinIcon("rufin-document-edit-symbolic", translate("Edit metadata"))
                    }
                })
            }) { padding ->
                if (failure != null) Column(Modifier.padding(padding).padding(24.dp)) {
                    ErrorNotice(failure!!)
                    TextButton({ revision++ }) { Text(translate("Retry")) }
                } else data?.let { details ->
                    val artwork = rememberArtwork(details.artworkBinding, player, with(LocalDensity.current) { 86.dp.roundToPx() })
                    Column(Modifier.fillMaxSize().padding(padding).verticalScroll(rememberScrollState())
                        .padding(horizontal = 20.dp, vertical = 24.dp), verticalArrangement = Arrangement.spacedBy(24.dp)) {
                        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(16.dp)) {
                            Artwork(artwork, Modifier.size(76.dp).clip(RoundedCornerShape(10.dp)))
                            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                                Text(details.title, style = MaterialTheme.typography.headlineSmall)
                                if (details.subtitle.isNotBlank()) Text(details.subtitle,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant, style = MaterialTheme.typography.bodyMedium)
                            }
                        }
                        if (details.audioSummary.isNotEmpty()) Surface(shape = RoundedCornerShape(12.dp), color = MaterialTheme.colorScheme.surfaceContainer) {
                            Row(Modifier.fillMaxWidth().padding(vertical = 16.dp, horizontal = 4.dp)) {
                                details.audioSummary.forEach { field ->
                                    Column(Modifier.weight(1f), horizontalAlignment = Alignment.CenterHorizontally,
                                        verticalArrangement = Arrangement.spacedBy(4.dp)) {
                                        Text(field.value, style = MaterialTheme.typography.titleSmall)
                                        Text(field.label, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                                    }
                                }
                            }
                        }
                        details.sections.forEach { section ->
                            SettingsGroup(section.title) {
                                section.fields.forEach { field ->
                                    Surface(color = MaterialTheme.colorScheme.surfaceContainer) {
                                        Row(Modifier.fillMaxWidth().padding(start = 14.dp, end = 6.dp, top = 8.dp, bottom = 8.dp),
                                            verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                                            Text(field.label, Modifier.weight(.85f), style = MaterialTheme.typography.bodySmall,
                                                color = MaterialTheme.colorScheme.onSurfaceVariant)
                                            Row(Modifier.weight(1.4f), verticalAlignment = Alignment.CenterVertically,
                                                horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                                                SelectionContainer(Modifier.weight(1f)) {
                                                    val linkColor = MaterialTheme.colorScheme.primary
                                                    Text(buildAnnotatedString {
                                                        if (field.links.isEmpty()) append(field.value)
                                                        else field.links.forEachIndexed { index, part ->
                                                            val next = part.target
                                                            if (next == null) append(part.text)
                                                            else withLink(LinkAnnotation.Clickable(index.toString(),
                                                                TextLinkStyles(SpanStyle(color = linkColor))) {
                                                                history = history + target
                                                                target = next
                                                            }) { append(part.text) }
                                                        }
                                                    }, style = MaterialTheme.typography.bodyMedium)
                                                }
                                                if (field.copy) CopyIconButton("Copy", clipboardLabel = field.label) { copy -> copy(field.value) }
                                                field.musicbrainzUrl?.let { url ->
                                                    val openLabel = translate("Open on MusicBrainz")
                                                    IconButton({ model.runAction {
                                                        context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)))
                                                    } }, Modifier.semantics { contentDescription = openLabel }) {
                                                        DetailLinkIcon("io.github.screwys.Rufin.external.musicbrainz", Modifier.size(24.dp))
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                } ?: Box(Modifier.fillMaxSize().padding(padding), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
            }
        }
        if (editing) MetadataEditorScreen(model, kind, mediaUri) { editing = false; revision++ }
    }
}
