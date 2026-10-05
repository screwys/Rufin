package io.github.screwys.rufin.platform

import android.app.RecoverableSecurityException
import android.content.ContentUris
import android.content.ContentValues
import android.content.ContentResolver
import android.content.Context
import android.content.IntentSender
import android.content.pm.PackageManager
import android.database.Cursor
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.provider.DocumentsContract
import android.provider.MediaStore
import android.provider.OpenableColumns
import android.util.AtomicFile
import android.system.ErrnoException
import android.system.Os
import android.system.OsConstants
import org.json.JSONArray
import org.json.JSONObject
import java.io.Closeable
import java.io.File
import java.io.FileNotFoundException
import java.io.RandomAccessFile
import java.nio.ByteBuffer
import java.nio.channels.FileChannel
import java.security.MessageDigest
import java.security.SecureRandom
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicLong

private const val DOCUMENT_PAGE_SIZE = 128

/** ContentResolver keeps provider identifiers and asset descriptor ownership here. */
class AndroidDocuments(
    private val context: Context,
    private val requestConsent: (IntentSender) -> Boolean = { false },
    private val requestPermission: (String) -> Boolean = { false },
) : Closeable {
    private val resolver = context.contentResolver
    private val nextHandle = AtomicLong(1)
    private val handles = ConcurrentHashMap<Long, OpenDocument>()
    private val rootSeed: ByteArray by lazy {
        val seed = AtomicFile(File(context.noBackupFilesDir, "document-root-seed"))
        try {
            seed.openRead().use { it.readBytes() }
        } catch (_: FileNotFoundException) {
            val bytes = ByteArray(32).also { SecureRandom().nextBytes(it) }
            val output = seed.startWrite()
            try {
                output.write(bytes)
                seed.finishWrite(output)
            } catch (error: Exception) {
                seed.failWrite(output)
                throw error
            }
            bytes
        }
    }

    private class OpenDocument(
        val channel: FileChannel,
        val owner: Closeable,
        val offset: Long,
        val length: Long,
        val temporary: File? = null,
    ) : Closeable {
        override fun close() {
            owner.close()
            temporary?.delete()
        }
    }

    fun request(text: String): String = requestOnce(text, true)

    private fun requestOnce(text: String, canPrompt: Boolean): String {
        val request = JSONObject(text)
        val uri = Uri.parse(request.getString("uri"))
        return try {
            when (request.getString("op")) {
                "stat" -> stat(uri)
                "identity" -> {
                    val identity = try { stat(uri).getString("native_id") }
                        catch (_: FileNotFoundException) { unavailableIdentity(uri) }
                        catch (_: SecurityException) { unavailableIdentity(uri) }
                    JSONObject().put("native_id", identity)
                }
                "root" -> {
                    val entry = stat(uri)
                    val hash = MessageDigest.getInstance("SHA-256")
                    hash.update(rootSeed)
                    val id = hash.digest(uri.toString().toByteArray(Charsets.UTF_8)).joinToString("") { "%02x".format(it) }
                    JSONObject().put("id", id).put("uri", uri.toString()).put("name", entry.getString("name"))
                }
                "list" -> list(uri, request.optLong("offset"))
                "open" -> open(uri)
                "prefix" -> {
                    val asset = openAsset(uri)
                    val input = try { asset.createInputStream() } catch (error: Exception) { asset.close(); throw error }
                    val bytes = ByteArray(request.getInt("length"))
                    var count = 0
                    input.use {
                        while (count < bytes.size) {
                            val read = it.read(bytes, count, bytes.size - count)
                            if (read <= 0) break
                            count += read
                        }
                    }
                    JSONObject().put("bytes", android.util.Base64.encodeToString(bytes.copyOf(count), android.util.Base64.NO_WRAP))
                }
                "copy" -> {
                    val asset = openAsset(uri)
                    val input = try { asset.createInputStream() } catch (error: Exception) { asset.close(); throw error }
                    input.use { File(request.getString("destination")).outputStream().use { output -> it.copyTo(output) } }
                    JSONObject()
                }
                "resolve" -> resolveRelative(uri, request.getString("relative"), request.getJSONArray("roots"))
                "save" -> {
                    if (!request.isNull("revision") && stat(uri).optString("revision") != request.getString("revision")) {
                        return JSONObject().put("status", 412).put("error", "The document changed before saving").toString()
                    }
                    write(uri, File(request.getString("source")))
                    JSONObject()
                }
                "create", "mkdir" -> {
                    val name = request.getString("name")
                    val type = if (request.getString("op") == "mkdir") DocumentsContract.Document.MIME_TYPE_DIR
                        else request.getString("mime_type")
                    val created = if (isMedia(uri)) {
                        resolver.insert(uri, ContentValues().apply {
                            put(MediaStore.MediaColumns.DISPLAY_NAME, name)
                            put(MediaStore.MediaColumns.MIME_TYPE, type)
                        })
                    } else DocumentsContract.createDocument(resolver, documentUri(uri), type, name)
                    created ?: throw FileNotFoundException("The provider could not create the document")
                    if (request.has("source")) {
                        try {
                            write(created, File(request.getString("source")))
                        } catch (failure: Exception) {
                            try {
                                if (isMedia(created)) resolver.delete(created, null, null)
                                else DocumentsContract.deleteDocument(resolver, created)
                            } catch (cleanup: Exception) { failure.addSuppressed(cleanup) }
                            throw failure
                        }
                    }
                    stat(created)
                }
                "rename", "move" -> {
                    var current = documentUri(uri)
                    if (request.getString("op") == "move" && !isMedia(current)) {
                        val parent = documentUri(Uri.parse(request.getString("parent")))
                        val grants = resolver.persistedUriPermissions.map { it.uri } + listOf(current).filter(DocumentsContract::isTreeUri)
                        val oldParent = findPath(current, grants).dropLast(1).lastOrNull()
                        if (oldParent != null && oldParent != parent) {
                            current = DocumentsContract.moveDocument(resolver, current, oldParent, parent)
                                ?: throw FileNotFoundException("The provider could not move the document")
                        }
                    }
                    val name = request.getString("name")
                    if (isMedia(current)) {
                        resolver.update(current, ContentValues().apply { put(MediaStore.MediaColumns.DISPLAY_NAME, name) }, null, null)
                    } else {
                        current = DocumentsContract.renameDocument(resolver, current, name)
                            ?: throw FileNotFoundException("The provider could not rename the document")
                    }
                    stat(current)
                }
                "delete" -> {
                    if (isMedia(uri)) resolver.delete(uri, null, null)
                    else if (!DocumentsContract.deleteDocument(resolver, documentUri(uri))) {
                        throw FileNotFoundException("The provider could not delete the document")
                    }
                    JSONObject()
                }
                else -> error("Unknown document operation")
            }.toString()
        } catch (error: FileNotFoundException) {
            JSONObject().put("status", 404).put("error", error.message).toString()
        } catch (error: SecurityException) {
            if (canPrompt && Build.VERSION.SDK_INT < 29 && isMedia(uri) && request.getString("op") in setOf("save", "rename", "delete", "move", "create")) {
                if (requestPermission(android.Manifest.permission.WRITE_EXTERNAL_STORAGE)) return requestOnce(text, false)
            } else if (canPrompt && Build.VERSION.SDK_INT >= 29 && error is RecoverableSecurityException) {
                if (requestConsent(error.userAction.actionIntent.intentSender)) return requestOnce(text, false)
            } else if (canPrompt && Build.VERSION.SDK_INT >= 30 && isMedia(uri) && request.getString("op") in setOf("save", "rename", "delete", "move")) {
                val consent = if (request.getString("op") == "delete") MediaStore.createDeleteRequest(resolver, listOf(uri))
                    else MediaStore.createWriteRequest(resolver, listOf(uri))
                if (requestConsent(consent.intentSender)) {
                    if (request.getString("op") == "delete") return JSONObject().toString()
                    return requestOnce(text, false)
                }
            }
            JSONObject().put("status", 403).put("error", error.message ?: "Document permission is required").toString()
        } catch (error: Exception) {
            JSONObject().put("error", error.message ?: error.javaClass.simpleName).toString()
        }
    }

    private fun isMedia(uri: Uri) = uri.authority == MediaStore.AUTHORITY
    private fun isMediaCollection(uri: Uri) = isMedia(uri) && uri.lastPathSegment?.toLongOrNull() == null

    private fun documentUri(uri: Uri): Uri =
        if (DocumentsContract.isTreeUri(uri) && !DocumentsContract.isDocumentUri(context, uri)) {
            DocumentsContract.buildDocumentUriUsingTree(uri, DocumentsContract.getTreeDocumentId(uri))
        } else uri

    private fun projection(uri: Uri) = if (isMedia(uri)) {
        arrayOf(
        MediaStore.MediaColumns._ID, MediaStore.MediaColumns.DISPLAY_NAME,
        MediaStore.MediaColumns.MIME_TYPE, MediaStore.MediaColumns.SIZE, MediaStore.MediaColumns.DATE_MODIFIED,
        ) + if (Build.VERSION.SDK_INT >= 29) arrayOf(MediaStore.MediaColumns.VOLUME_NAME) else emptyArray()
    } else arrayOf(
        DocumentsContract.Document.COLUMN_DOCUMENT_ID, DocumentsContract.Document.COLUMN_DISPLAY_NAME,
        DocumentsContract.Document.COLUMN_MIME_TYPE, DocumentsContract.Document.COLUMN_SIZE,
        DocumentsContract.Document.COLUMN_LAST_MODIFIED, DocumentsContract.Document.COLUMN_FLAGS,
    )

    private fun stat(uri: Uri): JSONObject {
        if (isMediaCollection(uri)) return JSONObject()
            .put("uri", uri.toString()).put("name", "Music").put("directory", true)
            .put("native_id", nativeIdentity("collection", uri.toString()))
            .put("mime_type", DocumentsContract.Document.MIME_TYPE_DIR).put("size", JSONObject.NULL)
            .put("revision", JSONObject.NULL).put("writable", false).put("deletable", false).put("can_create", true)
        val document = documentUri(uri)
        if (isMedia(document) || DocumentsContract.isDocumentUri(context, document)) {
            resolver.query(document, projection(document), null, null, null)?.use { cursor ->
                if (cursor.moveToFirst()) return entry(cursor, document, false)
            }
            throw FileNotFoundException("The document is unavailable")
        }
        // ACTION_OPEN_DOCUMENT can also return a non-DocumentsProvider URI.
        resolver.query(document, arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE), null, null, null)?.use { cursor ->
            if (cursor.moveToFirst()) {
                val writable = context.checkUriPermission(document, android.os.Process.myPid(), android.os.Process.myUid(), android.content.Intent.FLAG_GRANT_WRITE_URI_PERMISSION) == PackageManager.PERMISSION_GRANTED
                return JSONObject().put("uri", document.toString()).put("name", cursor.getString(0))
                    .put("native_id", nativeIdentity("uri", document.toString()))
                    .put("directory", false).put("mime_type", resolver.getType(document) ?: "application/octet-stream")
                    .put("size", if (cursor.isNull(1)) JSONObject.NULL else cursor.getLong(1))
                    .put("revision", JSONObject.NULL).put("writable", writable).put("deletable", writable).put("can_create", false)
            }
        }
        throw FileNotFoundException("The document is unavailable")
    }

    private fun entry(cursor: Cursor, parent: Uri, child: Boolean): JSONObject {
        val media = isMedia(parent)
        val name = cursor.getString(1)
        val type = cursor.getString(2) ?: "application/octet-stream"
        val size = if (cursor.isNull(3)) null else cursor.getLong(3)
        val modified = if (cursor.isNull(4)) null else cursor.getLong(4)
        val flags = if (media) 0 else cursor.getInt(5)
        val uri = if (!child) parent else if (media) ContentUris.withAppendedId(parent, cursor.getLong(0))
            else DocumentsContract.buildDocumentUriUsingTree(parent, cursor.getString(0))
        val writable = media || flags and DocumentsContract.Document.FLAG_SUPPORTS_WRITE != 0
        val identity = if (media) {
            val volume = if (Build.VERSION.SDK_INT >= 29) cursor.getString(5) else parent.pathSegments.first()
            nativeIdentity("media", volume, cursor.getLong(0).toString())
        } else {
            val equivalent = if (type != DocumentsContract.Document.MIME_TYPE_DIR) equivalentMediaIdentity(uri) else null
            equivalent ?: nativeIdentity("document", parent.authority.orEmpty(), cursor.getString(0))
        }
        return JSONObject().put("uri", uri.toString()).put("name", name)
            .put("native_id", identity)
            .put("directory", type == DocumentsContract.Document.MIME_TYPE_DIR).put("mime_type", type)
            .put("size", size ?: JSONObject.NULL)
            .put("revision", if (modified != null && modified > 0) "$modified:${size ?: -1}" else JSONObject.NULL)
            .put("writable", writable).put("deletable", media || flags and DocumentsContract.Document.FLAG_SUPPORTS_DELETE != 0)
            .put("can_create", flags and DocumentsContract.Document.FLAG_DIR_SUPPORTS_CREATE != 0)
    }

    private fun equivalentMediaIdentity(uri: Uri): String? {
        if (Build.VERSION.SDK_INT < 29) return null
        // Android only defines this conversion for these system providers.
        val supported = uri.authority == "com.android.externalstorage.documents" ||
            (Build.VERSION.SDK_INT >= 31 && uri.authority == "com.android.providers.media.documents")
        if (!supported) return null
        return try {
            val media = MediaStore.getMediaUri(context, uri) ?: return null
            val volume = MediaStore.getVolumeName(media)
            if (volume != MediaStore.VOLUME_EXTERNAL) {
                nativeIdentity("media", volume, ContentUris.parseId(media).toString())
            } else {
                // The merged external collection can include several storage volumes.
                resolver.query(media, arrayOf(MediaStore.MediaColumns.VOLUME_NAME, MediaStore.MediaColumns._ID), null, null, null)?.use { cursor ->
                    if (cursor.moveToFirst()) nativeIdentity("media", cursor.getString(0), cursor.getLong(1).toString()) else null
                }
            }
        } catch (_: IllegalArgumentException) {
            // The provider can expose a file before MediaStore has indexed it.
            null
        } catch (_: SecurityException) {
            // A document grant remains usable even when media lookup is unavailable.
            null
        }
    }

    private fun unavailableIdentity(uri: Uri): String {
        val document = documentUri(uri)
        return when {
            DocumentsContract.isDocumentUri(context, document) ->
                nativeIdentity("document", document.authority.orEmpty(), DocumentsContract.getDocumentId(document))
            isMedia(document) && !isMediaCollection(document) -> {
                val volume = if (Build.VERSION.SDK_INT >= 29) MediaStore.getVolumeName(document) else document.pathSegments.first()
                nativeIdentity("media", volume, ContentUris.parseId(document).toString())
            }
            else -> nativeIdentity("uri", document.toString())
        }
    }

    private fun nativeIdentity(vararg parts: String): String {
        val hash = MessageDigest.getInstance("SHA-256")
        hash.update(rootSeed)
        for (part in parts) {
            val bytes = part.toByteArray(Charsets.UTF_8)
            hash.update(ByteBuffer.allocate(4).putInt(bytes.size).array())
            hash.update(bytes)
        }
        return hash.digest().joinToString("") { "%02x".format(it) }
    }

    private fun list(uri: Uri, offset: Long): JSONObject {
        val parent = documentUri(uri)
        if (isMediaCollection(parent)) return listMedia(parent, offset)
        val children = DocumentsContract.buildChildDocumentsUriUsingTree(parent, DocumentsContract.getDocumentId(parent))
        val entries = JSONArray()
        var next: Long? = null
        var position = offset.toInt()
        val paged = if (Build.VERSION.SDK_INT >= 26) {
            val arguments = Bundle().apply {
                putInt(ContentResolver.QUERY_ARG_LIMIT, DOCUMENT_PAGE_SIZE)
                putInt(ContentResolver.QUERY_ARG_OFFSET, position)
            }
            val candidate = try { resolver.query(children, projection(parent), arguments, null) }
                catch (_: IllegalArgumentException) { null }
                catch (_: UnsupportedOperationException) { null }
            val honored = candidate?.extras?.getStringArray(ContentResolver.EXTRA_HONORED_ARGS).orEmpty()
            if (ContentResolver.QUERY_ARG_LIMIT in honored && ContentResolver.QUERY_ARG_OFFSET in honored) {
                position = 0
                candidate
            } else {
                candidate?.close()
                null
            }
        } else null
        (paged ?: resolver.query(children, projection(parent), null, null, null))?.use { cursor ->
            if (cursor.moveToPosition(position)) {
                do {
                    entries.put(entry(cursor, parent, true))
                    if (entries.length() == DOCUMENT_PAGE_SIZE) {
                        if (paged == null && cursor.moveToNext()) next = offset + DOCUMENT_PAGE_SIZE
                        break
                    }
                } while (cursor.moveToNext())
            }
        } ?: throw FileNotFoundException("The folder is unavailable")
        if (paged != null && entries.length() > 0) next = offset + entries.length()
        return JSONObject().put("entries", entries).put("next", next ?: JSONObject.NULL)
    }

    private fun listMedia(parent: Uri, afterId: Long): JSONObject {
        val id = MediaStore.MediaColumns._ID
        val selection = if (afterId == 0L) null else "$id > ?"
        val selectionArgs = if (afterId == 0L) null else arrayOf(afterId.toString())
        val result = if (Build.VERSION.SDK_INT >= 30) {
            val arguments = Bundle().apply {
                putInt(ContentResolver.QUERY_ARG_LIMIT, DOCUMENT_PAGE_SIZE + 1)
                putStringArray(ContentResolver.QUERY_ARG_SORT_COLUMNS, arrayOf(id))
                putInt(ContentResolver.QUERY_ARG_SORT_DIRECTION, ContentResolver.QUERY_SORT_DIRECTION_ASCENDING)
                if (selection != null) {
                    putString(ContentResolver.QUERY_ARG_SQL_SELECTION, selection)
                    putStringArray(ContentResolver.QUERY_ARG_SQL_SELECTION_ARGS, selectionArgs)
                }
            }
            resolver.query(parent, projection(parent), arguments, null)
        } else {
            // MediaProvider used the URI limit through Android 10, including API 26-29.
            val limited = parent.buildUpon().appendQueryParameter("limit", (DOCUMENT_PAGE_SIZE + 1).toString()).build()
            resolver.query(limited, projection(parent), selection, selectionArgs, "$id ASC")
        }
        val entries = JSONArray()
        var next: Long? = null
        result?.use { cursor ->
            var lastId = afterId
            while (entries.length() < DOCUMENT_PAGE_SIZE && cursor.moveToNext()) {
                lastId = cursor.getLong(0)
                entries.put(entry(cursor, parent, true))
            }
            if (entries.length() == DOCUMENT_PAGE_SIZE && cursor.moveToNext()) next = lastId
        } ?: throw FileNotFoundException("The media library is unavailable")
        return JSONObject().put("entries", entries).put("next", next ?: JSONObject.NULL)
    }

    private fun openAsset(uri: Uri): android.content.res.AssetFileDescriptor {
        val document = documentUri(uri)
        val virtual = if (DocumentsContract.isDocumentUri(context, document)) {
            resolver.query(document, arrayOf(DocumentsContract.Document.COLUMN_FLAGS), null, null, null)?.use {
                it.moveToFirst() && it.getInt(0) and DocumentsContract.Document.FLAG_VIRTUAL_DOCUMENT != 0
            } == true
        } else false
        val asset = if (virtual) {
            val types = resolver.getStreamTypes(document, "*/*")
                ?: throw FileNotFoundException("The virtual document has no readable representation")
            val type = types.firstOrNull { it.startsWith("audio/") } ?: types.first()
            resolver.openTypedAssetFileDescriptor(document, type, null)
        } else resolver.openAssetFileDescriptor(document, "r")
        return asset ?: throw FileNotFoundException("The document could not be opened")
    }

    private fun open(uri: Uri): JSONObject {
        val asset = openAsset(uri)
        val stream = try { asset.createInputStream() } catch (error: Exception) { asset.close(); throw error }
        val seekable = try {
            Os.lseek(asset.fileDescriptor, 0, OsConstants.SEEK_CUR)
            true
        } catch (error: ErrnoException) {
            if (error.errno == OsConstants.ESPIPE) false else { stream.close(); throw error }
        }
        val opened = if (seekable) {
            try {
                val channel = stream.channel
                val length = if (asset.declaredLength >= 0) asset.declaredLength else channel.size() - asset.startOffset
                OpenDocument(channel, stream, asset.startOffset, length)
            } catch (error: Exception) { stream.close(); throw error }
        } else {
            val file = try { File.createTempFile("document-", ".data", context.cacheDir) }
                catch (error: Exception) { stream.close(); throw error }
            try {
                stream.use { input -> file.outputStream().use { output -> input.copyTo(output) } }
                val copy = RandomAccessFile(file, "r")
                OpenDocument(copy.channel, copy, 0, copy.length(), file)
            } catch (error: Exception) {
                file.delete()
                throw error
            }
        }
        val handle = nextHandle.getAndIncrement()
        handles[handle] = opened
        return JSONObject().put("handle", handle).put("length", opened.length)
    }

    fun read(handle: Long, offset: Long, length: Int): ByteArray {
        val document = handles[handle] ?: throw FileNotFoundException("The document handle was closed")
        val count = minOf(length.toLong(), (document.length - offset).coerceAtLeast(0)).toInt()
        val bytes = ByteArray(count)
        val buffer = ByteBuffer.wrap(bytes)
        var position = document.offset + offset
        while (buffer.hasRemaining()) {
            val read = document.channel.read(buffer, position)
            if (read <= 0) break
            position += read
        }
        return if (buffer.hasRemaining()) bytes.copyOf(buffer.position()) else bytes
    }

    fun close(handle: Long) { handles.remove(handle)?.close() }
    override fun close() { handles.keys.toList().forEach(::close) }

    private fun write(uri: Uri, source: File) {
        resolver.openOutputStream(documentUri(uri), "wt")?.use { output ->
            source.inputStream().use { it.copyTo(output) }
        } ?: throw FileNotFoundException("The document could not be opened for writing")
    }

    private fun findPath(uri: Uri, roots: List<Uri>): List<Uri> {
        if (Build.VERSION.SDK_INT >= 26 && DocumentsContract.isTreeUri(uri)) {
            try {
                DocumentsContract.findDocumentPath(resolver, uri)?.path?.let { ids ->
                    return ids.map { DocumentsContract.buildDocumentUriUsingTree(uri, it) }
                }
            } catch (_: UnsupportedOperationException) { }
        }
        for (root in roots.filter(DocumentsContract::isTreeUri)) {
            val rootDocument = documentUri(root)
            if (rootDocument == uri) return listOf(rootDocument)
            val pending = ArrayDeque<Pair<List<Uri>, Long>>()
            pending.addLast(listOf(rootDocument) to 0L)
            while (pending.isNotEmpty()) {
                val (parents, offset) = pending.removeLast()
                val page = list(parents.last(), offset)
                if (!page.isNull("next")) pending.addLast(parents to page.getLong("next"))
                val entries = page.getJSONArray("entries")
                for (index in 0 until entries.length()) {
                    val child = entries.getJSONObject(index)
                    val childUri = Uri.parse(child.getString("uri"))
                    val path = parents + childUri
                    if (childUri == uri || sameDocument(childUri, uri)) return path
                    if (child.getBoolean("directory")) pending.addLast(path to 0L)
                }
            }
        }
        throw SecurityException("Select the containing folder to access related documents")
    }

    private fun sameDocument(first: Uri, second: Uri): Boolean =
        first.authority == second.authority && DocumentsContract.isDocumentUri(context, first) &&
            DocumentsContract.isDocumentUri(context, second) &&
            DocumentsContract.getDocumentId(first) == DocumentsContract.getDocumentId(second)

    private fun resolveRelative(uri: Uri, relative: String, roots: JSONArray): JSONObject {
        if (isMedia(uri)) return resolveMediaRelative(uri, relative)
        val grants = (0 until roots.length()).map { Uri.parse(roots.getJSONObject(it).getString("uri")) }
        val parents = findPath(documentUri(uri), grants).dropLast(1).toMutableList()
        for (part in relative.replace('\\', '/').split('/')) {
            when (part) {
                "", "." -> continue
                ".." -> {
                    if (parents.size <= 1) throw SecurityException("The relative file is outside the selected folder")
                    parents.removeAt(parents.lastIndex)
                }
                else -> {
                    val parent = parents.lastOrNull() ?: throw SecurityException("Select the containing folder to access related documents")
                    var offset = 0L
                    var found: Uri? = null
                    while (found == null) {
                        val page = list(parent, offset)
                        val entries = page.getJSONArray("entries")
                        for (index in 0 until entries.length()) {
                            val entry = entries.getJSONObject(index)
                            if (entry.getString("name") == part) {
                                found = Uri.parse(entry.getString("uri"))
                                break
                            }
                        }
                        if (found != null || page.isNull("next")) break
                        offset = page.getLong("next")
                    }
                    parents.add(found ?: throw FileNotFoundException("The related document was not found"))
                }
            }
        }
        return stat(parents.lastOrNull() ?: throw FileNotFoundException("The related document was not found"))
    }

    private fun resolveMediaRelative(uri: Uri, relative: String): JSONObject {
        val column = if (Build.VERSION.SDK_INT >= 29) MediaStore.MediaColumns.RELATIVE_PATH else MediaStore.MediaColumns.DATA
        val directory = resolver.query(uri, arrayOf(column), null, null, null)?.use { cursor ->
            if (!cursor.moveToFirst()) throw FileNotFoundException("The document is unavailable")
            val path = cursor.getString(0) ?: throw FileNotFoundException("The provider did not report a containing folder")
            if (Build.VERSION.SDK_INT >= 29) path else path.substringBeforeLast('/') + "/"
        } ?: throw FileNotFoundException("The document is unavailable")
        val parts = directory.trim('/').split('/').filter { it.isNotEmpty() }.toMutableList()
        for (part in relative.replace('\\', '/').split('/')) {
            when (part) {
                "", "." -> continue
                ".." -> {
                    if (parts.isEmpty()) throw SecurityException("The relative document is outside the storage volume")
                    parts.removeAt(parts.lastIndex)
                }
                else -> parts.add(part)
            }
        }
        val name = parts.removeAt(parts.lastIndex)
        val folder = parts.joinToString("/") + "/"
        val collection = MediaStore.Files.getContentUri(
            if (Build.VERSION.SDK_INT >= 29) MediaStore.getVolumeName(uri) else uri.pathSegments.first(),
        )
        val (selection, arguments) = if (Build.VERSION.SDK_INT >= 29) {
            "${MediaStore.MediaColumns.RELATIVE_PATH}=? AND ${MediaStore.MediaColumns.DISPLAY_NAME}=?" to arrayOf(folder, name)
        } else {
            "${MediaStore.MediaColumns.DATA}=?" to arrayOf("/$folder$name")
        }
        resolver.query(collection, arrayOf(MediaStore.MediaColumns._ID), selection, arguments, null)?.use { cursor ->
            if (cursor.moveToFirst()) return stat(ContentUris.withAppendedId(collection, cursor.getLong(0)))
        }
        throw FileNotFoundException("The related document is unavailable; select its containing folder to grant access")
    }
}
