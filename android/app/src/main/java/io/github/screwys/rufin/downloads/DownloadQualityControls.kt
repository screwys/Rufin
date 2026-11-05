package io.github.screwys.rufin.downloads

import androidx.compose.runtime.Composable
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.settings.PreferenceChoice
import io.github.screwys.rufin.settings.SettingsSliderNumber
import io.github.screwys.rufin.shared.library.*
import io.github.screwys.rufin.shared.sources.DownloadQuality
import io.github.screwys.rufin.shared.rufin_core.downloadBitrates
import io.github.screwys.rufin.shared.rufin_core.downloadQualityShowsBitrate
import io.github.screwys.rufin.shared.rufin_core.downloadQualityShowsChannels
import io.github.screwys.rufin.shared.rufin_core.downloadQualityWithBitrate
import io.github.screwys.rufin.shared.rufin_core.downloadQualityWithChannels
import io.github.screwys.rufin.shared.rufin_core.downloadQualityWithCodec

@Composable
internal fun DownloadQualityControls(quality: DownloadQuality, enabled: Boolean = true, change: (DownloadQuality) -> Unit) {
    val encoding = (quality as? DownloadQuality.Transcoded)?.v1
    PreferenceChoice(translate("Format"), encoding?.codec,
        listOf<Pair<AudioCodec?, String>>(null to translate("Original")) + AudioCodec.entries.map { it to it.name },
        enabled = { enabled }, select = { codec -> change(downloadQualityWithCodec(quality, codec)) })
    if (downloadQualityShowsBitrate(quality)) {
        val rates = downloadBitrates()
        val rate = encoding?.bitrateKbps ?: 128u
        SettingsSliderNumber(translate("Bitrate"), rate.toDouble(),
            minOf(rates.first(), rate).toDouble()..maxOf(rates.last(), rate).toDouble(),
            (rates[1] - rates[0]).toDouble(), integer = true, unit = "kbps", enabled = enabled) {
            change(downloadQualityWithBitrate(quality, (it as Number).toInt().toUInt()))
        }
    }
    if (downloadQualityShowsChannels(quality)) PreferenceChoice(translate("Channels"), encoding?.channels,
        listOf<Pair<AudioChannels?, String>>(AudioChannels.PRESERVE to translate("Preserve channels"),
            AudioChannels.STEREO_LOSSY to translate("Stereo for lossy formats"), AudioChannels.STEREO to translate("Stereo")),
        enabled = { enabled }, select = { channels -> channels?.let { change(downloadQualityWithChannels(quality, it)) } })
}
