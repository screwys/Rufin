# JNA reflects native methods, structures and callback interfaces.
-keep class com.sun.jna.** { *; }
# JNA's optional desktop AWT helpers are not used on Android.
-dontwarn java.awt.Component
-dontwarn java.awt.GraphicsEnvironment
-dontwarn java.awt.HeadlessException
-dontwarn java.awt.Window
-keepattributes RuntimeVisibleAnnotations
-keep class io.github.screwys.rufin.core.** extends com.sun.jna.Structure { *; }
-keep interface io.github.screwys.rufin.core.** extends com.sun.jna.Callback { *; }
-keepclassmembers class io.github.screwys.rufin.core.** implements com.sun.jna.Callback {
    public *;
}

# These helpers are constructed or called by name from native code.
-keep class io.github.screwys.rufin.platform.AndroidAudioTrack { *; }
-keep class io.github.screwys.rufin.platform.AndroidNetwork { *; }
-keep class io.github.screwys.rufin.platform.AndroidDiscoveryClient { *; }
-keep class io.github.screwys.rufin.platform.AndroidDocuments { *; }
-keep class io.github.screwys.rufin.platform.AndroidKeyring { *; }
-keep class org.freedesktop.gstreamer.** { *; }
-keep class org.rustls.platformverifier.** { *; }
