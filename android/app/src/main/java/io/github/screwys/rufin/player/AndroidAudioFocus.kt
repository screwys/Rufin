package io.github.screwys.rufin.player

import android.content.Context
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.os.Build
import android.os.Handler
import android.os.Looper

internal class AndroidAudioFocus(
    context: Context,
    private val isPlayingLocally: () -> Boolean,
    private val pause: () -> Unit,
    private val resume: () -> Unit,
) : AutoCloseable {
    private val manager = context.getSystemService(AudioManager::class.java)
    var hasFocus = false
        private set
    var resumeOnGain = false
        private set
    private var hasRequest = false
    private val listener = AudioManager.OnAudioFocusChangeListener { change ->
        when (change) {
            AudioManager.AUDIOFOCUS_GAIN -> {
                hasFocus = true
                val shouldResume = resumeOnGain
                resumeOnGain = false
                if (shouldResume) resume()
            }
            AudioManager.AUDIOFOCUS_LOSS_TRANSIENT,
            AudioManager.AUDIOFOCUS_LOSS_TRANSIENT_CAN_DUCK -> {
                hasFocus = false
                resumeOnGain = resumeOnGain || isPlayingLocally()
                if (isPlayingLocally()) pause()
            }
            AudioManager.AUDIOFOCUS_LOSS -> {
                abandon()
                if (isPlayingLocally()) pause()
            }
        }
    }
    private val request = if (Build.VERSION.SDK_INT >= 26) {
        AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
            .setAudioAttributes(AudioAttributes.Builder()
                .setUsage(AudioAttributes.USAGE_MEDIA)
                .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                .build())
            .setWillPauseWhenDucked(true)
            .setOnAudioFocusChangeListener(listener, Handler(Looper.getMainLooper()))
            .build()
    } else null

    @Suppress("DEPRECATION")
    fun acquire(): Boolean {
        resumeOnGain = false
        if (hasFocus) return true
        hasFocus = (if (Build.VERSION.SDK_INT >= 26) {
            manager.requestAudioFocus(request!!)
        } else {
            manager.requestAudioFocus(listener, AudioManager.STREAM_MUSIC, AudioManager.AUDIOFOCUS_GAIN)
        }) == AudioManager.AUDIOFOCUS_REQUEST_GRANTED
        hasRequest = hasFocus
        return hasFocus
    }

    @Suppress("DEPRECATION")
    fun abandon() {
        hasFocus = false
        resumeOnGain = false
        if (!hasRequest) return
        hasRequest = false
        if (Build.VERSION.SDK_INT >= 26) manager.abandonAudioFocusRequest(request!!)
        else manager.abandonAudioFocus(listener)
    }

    override fun close() = abandon()
}
