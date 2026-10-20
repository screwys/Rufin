include(FetchContent)

# The mobile SDK omits bs2b. Build its unchanged upstream sources alongside the SDK.
FetchContent_Declare(rufin_libbs2b
    URL https://downloads.sourceforge.net/project/bs2b/libbs2b/3.1.0/libbs2b-3.1.0.tar.gz
    URL_HASH SHA256=6aaafd81aae3898ee40148dd1349aab348db9bfae9767d0e66e0b07ddd4b2528
    DOWNLOAD_EXTRACT_TIMESTAMP TRUE)
FetchContent_Declare(rufin_gst_bad
    URL https://gstreamer.freedesktop.org/src/gst-plugins-bad/gst-plugins-bad-1.28.7.tar.xz
    URL_HASH SHA256=dc525383c18b2c265bbe6a43d498656cd918aaa130aa4e3abeabcdaa741c3ffe
    DOWNLOAD_EXTRACT_TIMESTAMP TRUE)
FetchContent_MakeAvailable(rufin_libbs2b rufin_gst_bad)

set(bs2b_include "${CMAKE_CURRENT_BINARY_DIR}/bs2b-include")
foreach(header bs2b.h bs2btypes.h bs2bversion.h)
    configure_file("${rufin_libbs2b_SOURCE_DIR}/src/${header}"
        "${bs2b_include}/bs2b/${header}" COPYONLY)
endforeach()
add_library(RufinBs2b STATIC
    "${rufin_libbs2b_SOURCE_DIR}/src/bs2b.c"
    "${rufin_gst_bad_SOURCE_DIR}/ext/bs2b/gstbs2b.c")
set_target_properties(RufinBs2b PROPERTIES POSITION_INDEPENDENT_CODE TRUE)
target_compile_definitions(RufinBs2b PRIVATE
    GST_PLUGIN_BUILD_STATIC
    VERSION="1.28.7"
    PACKAGE="gst-plugins-bad"
    GST_PACKAGE_NAME="GStreamer Bad Plug-ins"
    GST_PACKAGE_ORIGIN="https://gstreamer.freedesktop.org/")
target_include_directories(RufinBs2b PRIVATE "${bs2b_include}")
target_link_libraries(RufinBs2b PRIVATE GStreamer::api_audio m)

# FindGStreamer accepts supplied component targets and generates their static registration.
add_library(GStreamer::bs2b ALIAS RufinBs2b)
set(GStreamer_bs2b_FOUND TRUE)

configure_file("${rufin_libbs2b_SOURCE_DIR}/COPYING"
    "${GSTREAMER_ASSETS_DIR}/licenses/libbs2b-3.1.0.txt" COPYONLY)
configure_file("${rufin_gst_bad_SOURCE_DIR}/COPYING"
    "${GSTREAMER_ASSETS_DIR}/licenses/gst-plugins-bad-1.28.7.txt" COPYONLY)
