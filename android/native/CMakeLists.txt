cmake_minimum_required(VERSION 3.25)
project(RufinAndroid LANGUAGES C CXX)
list(APPEND CMAKE_MODULE_PATH "${GStreamer_ROOT_DIR}/share/cmake")
set(CMAKE_LIBRARY_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/libs")
set(GSTREAMER_JAVA_SRC_DIR "${CMAKE_BINARY_DIR}/java")
set(GSTREAMER_ASSETS_DIR "${CMAKE_BINARY_DIR}/assets")
include("${GStreamer_ROOT_DIR}/share/gst-android/ndk-build/plugins.cmake")
set(RUFIN_CODECS ${GSTREAMER_PLUGINS_CODECS})
# Keep audio, containers and cover-image codecs; omit standalone video components.
list(REMOVE_ITEM RUFIN_CODECS
    svtav1 rav1e dav1d vpx openh264 lcevcdecoder theora ffv1 flxdec y4m
    codecalpha codectimestamper ivfparse hsv rfbsrc cdg)
set(G_IO_MODULES openssl)
set(GStreamer_EXTRA_DEPS gio-2.0 gstreamer-controller-1.0)
find_package(GStreamerMobile REQUIRED COMPONENTS ca_certificates api_app api_audio api_pbutils
    coreelements adder app audioconvert audiomixer audiorate audioresample audiotestsrc gio
    pbtypes rawparse typefindfunctions volume autodetect ${GSTREAMER_PLUGINS_PLAYBACK}
    ${RUFIN_CODECS} ${GSTREAMER_PLUGINS_CODECS_GPL} ${GSTREAMER_PLUGINS_CODECS_RESTRICTED}
    soup tcp udp rtsp rtp rtpmanager srtp audiofx equalizer level replaygain spectrum
    aiff audiobuffersplit audiofxbad debugutilsbad insertbin soundtouch opensles)
target_compile_features(GStreamerMobile PRIVATE cxx_std_17)
target_link_options(GStreamerMobile PRIVATE -Wl,--no-undefined -Wl,--undefined=gst_audio_sink_get_type)
# SDK Rust plugin internals are not part of the app's shared-library interface.
target_link_options(GStreamerMobile PRIVATE
    -Wl,--exclude-libs,libgstrsworkspace.a
    -Wl,--exclude-libs,librsvg-2.a)
