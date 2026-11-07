package io.github.screwys.rufin.ui

/** A duration as a clock reads it: m:ss, or h:mm:ss from an hour on. */
internal fun clockText(millis: ULong): String {
    val seconds = millis / 1000UL
    return if (seconds >= 3600UL) "${seconds / 3600UL}:${((seconds / 60UL) % 60UL).toString().padStart(2, '0')}:${(seconds % 60UL).toString().padStart(2, '0')}"
    else "${seconds / 60UL}:${(seconds % 60UL).toString().padStart(2, '0')}"
}
