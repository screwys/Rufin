include(FetchContent)

# The mobile SDK omits these plugins. Build their unchanged upstream sources.
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
target_include_directories(RufinBs2b PRIVATE "${bs2b_include}")
target_link_libraries(RufinBs2b PRIVATE GStreamer::api_audio m)

FetchContent_Declare(rufin_chromaprint
    URL https://github.com/acoustid/chromaprint/archive/refs/tags/v1.6.0.tar.gz
    URL_HASH SHA256=65bfce4a35b2e673dbcda917b6aa577e2f145cf805243d19e6a50fea2a520c2a
    DOWNLOAD_EXTRACT_TIMESTAMP TRUE)
block()
    set(CMAKE_POLICY_DEFAULT_CMP0077 NEW)
    set(CMAKE_POLICY_DEFAULT_CMP0126 NEW)
    set(BUILD_SHARED_LIBS OFF)
    set(BUILD_TESTS OFF)
    set(BUILD_TOOLS OFF)
    set(CMAKE_POSITION_INDEPENDENT_CODE ON)
    set(FFT_LIB avtx)
    list(APPEND CMAKE_FIND_ROOT_PATH "${GStreamer_ROOT_DIR}")
    set(FFMPEG_ROOT "${GStreamer_ROOT_DIR}")
    FetchContent_MakeAvailable(rufin_chromaprint)
endblock()
FetchContent_GetProperties(rufin_chromaprint)
add_library(RufinChromaprint STATIC
    "${rufin_gst_bad_SOURCE_DIR}/ext/chromaprint/gstchromaprint.c")
target_include_directories(RufinChromaprint PRIVATE "${rufin_chromaprint_SOURCE_DIR}/src")
target_link_libraries(RufinChromaprint PRIVATE GStreamer::api_audio chromaprint)
foreach(plugin RufinBs2b RufinChromaprint)
    set_target_properties(${plugin} PROPERTIES POSITION_INDEPENDENT_CODE TRUE)
    target_compile_definitions(${plugin} PRIVATE
        GST_PLUGIN_BUILD_STATIC
        VERSION="1.28.7"
        PACKAGE="gst-plugins-bad"
        GST_PACKAGE_NAME="GStreamer Bad Plug-ins"
        GST_PACKAGE_ORIGIN="https://gstreamer.freedesktop.org/")
endforeach()

# FindGStreamer accepts supplied component targets and generates their static registration.
add_library(GStreamer::bs2b ALIAS RufinBs2b)
set(GStreamer_bs2b_FOUND TRUE)
add_library(GStreamer::chromaprint ALIAS RufinChromaprint)
set(GStreamer_chromaprint_FOUND TRUE)

configure_file("${rufin_libbs2b_SOURCE_DIR}/COPYING"
    "${GSTREAMER_ASSETS_DIR}/licenses/libbs2b-3.1.0.txt" COPYONLY)
configure_file("${rufin_gst_bad_SOURCE_DIR}/COPYING"
    "${GSTREAMER_ASSETS_DIR}/licenses/gst-plugins-bad-1.28.7.txt" COPYONLY)
configure_file("${rufin_chromaprint_SOURCE_DIR}/LICENSE.md"
    "${GSTREAMER_ASSETS_DIR}/licenses/chromaprint-1.6.0.txt" COPYONLY)
