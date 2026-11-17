package io.github.screwys.rufin.downloads

import androidx.compose.runtime.Composable
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.settings.PreferenceChoice
import io.github.screwys.rufin.settings.SettingsSliderNumber
import io.github.screwys.rufin.shared.library.*
import io.github.screwys.rufin.shared.library.AudioQuality
import io.github.screwys.rufin.shared.rufin_core.audioBitrateMarks
import io.github.screwys.rufin.shared.rufin_core.audioBitrates
import io.github.screwys.rufin.shared.rufin_core.audioCodecLabel
import io.github.screwys.rufin.shared.rufin_core.audioQualityShowsBitrate
import io.github.screwys.rufin.shared.rufin_core.audioQualityShowsChannels
import io.github.screwys.rufin.shared.rufin_core.audioQualityWithBitrate
import io.github.screwys.rufin.shared.rufin_core.audioQualityWithChannels
import io.github.screwys.rufin.shared.rufin_core.audioQualityWithCodec
import io.github.screwys.rufin.shared.rufin_core.defaultAudioBitrate

/**
 * The format, bitrate and channels of a download or stream quality. With [follows], a first
 * choice leaves the quality to the one set elsewhere, which is null.
 */
@Composable
internal fun AudioQualityControls(quality: AudioQuality?, enabled: Boolean = true, follows: Boolean = false, change: (AudioQuality?) -> Unit) {
    val encoding = (quality as? AudioQuality.Transcoded)?.v1
    val formats = listOfNotNull(("default" to translate("Default")).takeIf { follows }, "original" to translate("Original")) +
        AudioCodec.entries.map { it.name to audioCodecLabel(it) }
    PreferenceChoice(translate("Format"), if (quality == null) "default" else encoding?.codec?.name ?: "original", formats,
        enabled = { enabled }, select = { format ->
            change(if (format == "default") null
                else audioQualityWithCodec(quality ?: AudioQuality.Original, AudioCodec.entries.find { it.name == format }))
        })
    if (quality == null) return
    if (audioQualityShowsBitrate(quality)) {
        val rates = audioBitrates()
        val fallback = defaultAudioBitrate()
        val rate = encoding?.bitrateKbps ?: fallback
        SettingsSliderNumber(translate("Bitrate"), rate.toDouble(),
            minOf(rates.first(), rate).toDouble()..maxOf(rates.last(), rate).toDouble(),
            (rates[1] - rates[0]).toDouble(), unit = "kbps", default = fallback.toDouble(),
            marks = audioBitrateMarks().map { it.toDouble() }, enabled = enabled) {
            change(audioQualityWithBitrate(quality, it.toInt().toUInt()))
        }
    }
    if (audioQualityShowsChannels(quality)) PreferenceChoice(translate("Channels"), encoding?.channels,
        listOf<Pair<AudioChannels?, String>>(AudioChannels.PRESERVE to translate("Preserve channels"),
            AudioChannels.STEREO_LOSSY to translate("Stereo for lossy formats"), AudioChannels.STEREO to translate("Stereo")),
        enabled = { enabled }, select = { channels -> channels?.let { change(audioQualityWithChannels(quality, it)) } })
}
