package io.github.screwys.rufin.downloads

import androidx.compose.runtime.Composable
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.settings.PreferenceChoice
import io.github.screwys.rufin.settings.SettingsSliderNumber
import io.github.screwys.rufin.shared.library.*
import io.github.screwys.rufin.shared.sources.DownloadQuality
import io.github.screwys.rufin.shared.downloads.downloadBitrates

@Composable
internal fun DownloadQualityControls(quality: DownloadQuality, enabled: Boolean = true, change: (DownloadQuality) -> Unit) {
    val profile = (quality as? DownloadQuality.Transcoded)?.v1
    val selected = profile ?: AudioEncoding(AudioCodec.OPUS, 128u, AudioChannels.PRESERVE)
    PreferenceChoice(translate("Format"), profile?.codec?.name ?: "original",
        listOf("original" to translate("Original")) + AudioCodec.entries.map { it.name to it.name },
        enabled = { enabled }, select = { value ->
            change(if (value == "original") DownloadQuality.Original else {
                val codec = AudioCodec.valueOf(value)
                DownloadQuality.Transcoded(selected.copy(codec = codec, bitrateKbps = if (codec == AudioCodec.FLAC) null else selected.bitrateKbps ?: 128u))
            })
        })
    if (profile != null) {
        if (profile.codec != AudioCodec.FLAC) {
            val rates = downloadBitrates()
            val rate = profile.bitrateKbps ?: 128u
            SettingsSliderNumber(translate("Bitrate"), rate.toDouble(),
                minOf(rates.first(), rate).toDouble()..maxOf(rates.last(), rate).toDouble(),
                (rates[1] - rates[0]).toDouble(), integer = true, unit = "kbps", enabled = enabled) {
                change(DownloadQuality.Transcoded(profile.copy(bitrateKbps = (it as Number).toInt().toUInt())))
            }
        }
        PreferenceChoice(translate("Channels"), profile.channels.name,
            listOf(AudioChannels.PRESERVE.name to translate("Preserve channels"), AudioChannels.STEREO_LOSSY.name to translate("Stereo for lossy formats"), AudioChannels.STEREO.name to translate("Stereo")),
            enabled = { enabled }, select = { change(DownloadQuality.Transcoded(profile.copy(channels = AudioChannels.valueOf(it)))) })
    }
}
