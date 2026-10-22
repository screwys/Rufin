package io.github.screwys.rufin.ui

import android.content.Context
import android.content.Intent
import android.net.Uri
import android.provider.DocumentsContract

internal fun openMediaUri(context: Context, value: String) {
    var uri = Uri.parse(value)
    val intent = Intent(Intent.ACTION_VIEW)
    if (uri.scheme == "content") {
        if (DocumentsContract.isTreeUri(uri) && !DocumentsContract.isDocumentUri(context, uri)) {
            uri = DocumentsContract.buildDocumentUriUsingTree(uri, DocumentsContract.getTreeDocumentId(uri))
        }
        intent.setDataAndType(uri, DocumentsContract.Document.MIME_TYPE_DIR)
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_PREFIX_URI_PERMISSION)
    } else intent.data = uri
    context.startActivity(intent)
}
