package io.github.screwys.rufin.platform

import android.content.Context
import android.net.ConnectivityManager

internal class AndroidNetwork(context: Context) {
    private val connectivity = context.applicationContext.getSystemService(ConnectivityManager::class.java)

    fun dnsServers(): Array<ByteArray> = connectivity.getLinkProperties(connectivity.activeNetwork)
        ?.dnsServers.orEmpty().map { it.address }.toTypedArray()
}
