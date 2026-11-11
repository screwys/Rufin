package io.github.screwys.rufin.platform

import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager

/** The launcher icon. Each is an activity-alias in the manifest; Android keeps which one is enabled. */
internal enum class AppIcon(private val alias: String) {
    Colored("MainActivity"), Dark("DarkIconActivity"), Light("LightIconActivity");

    private fun component(context: Context) = ComponentName(context, "io.github.screwys.rufin.$alias")

    private fun enabled(context: Context) = when (context.packageManager.getComponentEnabledSetting(component(context))) {
        PackageManager.COMPONENT_ENABLED_STATE_ENABLED -> true
        PackageManager.COMPONENT_ENABLED_STATE_DEFAULT -> this == Colored
        else -> false
    }

    companion object {
        fun current(context: Context) = entries.firstOrNull { it.enabled(context) } ?: Colored

        /** Enables the new entry before disabling the others, so the launcher always has one. Android
         *  removes the task opened through a disabled entry, so Rufin reopens through the new one. */
        fun select(context: Context, icon: AppIcon) {
            for (entry in listOf(icon) + entries.filter { it != icon }) {
                context.packageManager.setComponentEnabledSetting(entry.component(context),
                    if (entry == icon) PackageManager.COMPONENT_ENABLED_STATE_ENABLED else PackageManager.COMPONENT_ENABLED_STATE_DISABLED,
                    PackageManager.DONT_KILL_APP)
            }
            context.startActivity(Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER).setComponent(icon.component(context))
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_MULTIPLE_TASK))
        }
    }
}
