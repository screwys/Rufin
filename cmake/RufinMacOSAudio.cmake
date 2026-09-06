include(ExternalProject)

find_program(RUFIN_BREW brew REQUIRED)
find_program(RUFIN_MESON meson REQUIRED)
foreach(RUFIN_AUDIO_LIBRARY IN ITEMS game-music-emu libopenmpt)
  execute_process(
    COMMAND "${RUFIN_BREW}" --prefix "${RUFIN_AUDIO_LIBRARY}"
    OUTPUT_VARIABLE RUFIN_AUDIO_PREFIX
    OUTPUT_STRIP_TRAILING_WHITESPACE
    COMMAND_ERROR_IS_FATAL ANY
  )
  if(RUFIN_AUDIO_LIBRARY STREQUAL "game-music-emu")
    set(RUFIN_GME_PREFIX "${RUFIN_AUDIO_PREFIX}")
  else()
    set(RUFIN_OPENMPT_PREFIX "${RUFIN_AUDIO_PREFIX}")
  endif()
endforeach()

set(RUFIN_GSTREAMER_DOWNLOAD_DIR "${CMAKE_CURRENT_BINARY_DIR}/downloads")
file(MAKE_DIRECTORY "${RUFIN_GSTREAMER_DOWNLOAD_DIR}")
execute_process(
  COMMAND "${PKG_CONFIG_EXECUTABLE}" --modversion gstreamer-1.0
  OUTPUT_VARIABLE RUFIN_GSTREAMER_VERSION
  OUTPUT_STRIP_TRAILING_WHITESPACE
  COMMAND_ERROR_IS_FATAL ANY
)

function(rufin_gstreamer_archive_hash component output_variable)
  set(RUFIN_CHECKSUM_FILE
    "${RUFIN_GSTREAMER_DOWNLOAD_DIR}/gst-plugins-${component}-${RUFIN_GSTREAMER_VERSION}.sha256sum")
  file(DOWNLOAD
    "https://gstreamer.freedesktop.org/src/gst-plugins-${component}/gst-plugins-${component}-${RUFIN_GSTREAMER_VERSION}.tar.xz.sha256sum"
    "${RUFIN_CHECKSUM_FILE}"
    STATUS RUFIN_CHECKSUM_STATUS
    TLS_VERIFY ON
  )
  list(GET RUFIN_CHECKSUM_STATUS 0 RUFIN_CHECKSUM_CODE)
  if(NOT RUFIN_CHECKSUM_CODE EQUAL 0)
    list(GET RUFIN_CHECKSUM_STATUS 1 RUFIN_CHECKSUM_MESSAGE)
    message(FATAL_ERROR "Could not download GStreamer checksum: ${RUFIN_CHECKSUM_MESSAGE}")
  endif()
  file(STRINGS "${RUFIN_CHECKSUM_FILE}" RUFIN_CHECKSUM_LINE LIMIT_COUNT 1)
  string(REGEX MATCH "^[0-9a-fA-F]+" RUFIN_CHECKSUM "${RUFIN_CHECKSUM_LINE}")
  string(LENGTH "${RUFIN_CHECKSUM}" RUFIN_CHECKSUM_LENGTH)
  if(NOT RUFIN_CHECKSUM_LENGTH EQUAL 64)
    message(FATAL_ERROR "Invalid GStreamer checksum in ${RUFIN_CHECKSUM_FILE}")
  endif()
  set(${output_variable} "${RUFIN_CHECKSUM}" PARENT_SCOPE)
endfunction()

rufin_gstreamer_archive_hash(good RUFIN_GSTREAMER_GOOD_HASH)
rufin_gstreamer_archive_hash(bad RUFIN_GSTREAMER_BAD_HASH)

set(RUFIN_WAVPACK_BUILD_DIR "${CMAKE_CURRENT_BINARY_DIR}/gst-plugins-good-build")
set(RUFIN_WAVPACK_PLUGIN "${RUFIN_WAVPACK_BUILD_DIR}/ext/wavpack/libgstwavpack.dylib")
ExternalProject_Add(rufin-gst-wavpack
  PREFIX "${CMAKE_CURRENT_BINARY_DIR}/gst-plugins-good"
  DOWNLOAD_DIR "${RUFIN_GSTREAMER_DOWNLOAD_DIR}"
  URL "https://gstreamer.freedesktop.org/src/gst-plugins-good/gst-plugins-good-${RUFIN_GSTREAMER_VERSION}.tar.xz"
  URL_HASH "SHA256=${RUFIN_GSTREAMER_GOOD_HASH}"
  BINARY_DIR "${RUFIN_WAVPACK_BUILD_DIR}"
  CONFIGURE_COMMAND "${RUFIN_MESON}" setup <BINARY_DIR> <SOURCE_DIR>
    -Dauto_features=disabled -Dwavpack=enabled --buildtype=release
  BUILD_COMMAND "${RUFIN_MESON}" compile -C <BINARY_DIR> gstwavpack
  INSTALL_COMMAND ""
  BUILD_BYPRODUCTS "${RUFIN_WAVPACK_PLUGIN}"
  USES_TERMINAL_DOWNLOAD TRUE
  USES_TERMINAL_BUILD TRUE
)

set(RUFIN_EXTRA_AUDIO_BUILD_DIR "${CMAKE_CURRENT_BINARY_DIR}/gst-plugins-bad-build")
set(RUFIN_GME_PLUGIN "${RUFIN_EXTRA_AUDIO_BUILD_DIR}/ext/gme/libgstgme.dylib")
set(RUFIN_OPENMPT_PLUGIN "${RUFIN_EXTRA_AUDIO_BUILD_DIR}/ext/openmpt/libgstopenmpt.dylib")
ExternalProject_Add(rufin-gst-extra-audio
  PREFIX "${CMAKE_CURRENT_BINARY_DIR}/gst-plugins-bad"
  DOWNLOAD_DIR "${RUFIN_GSTREAMER_DOWNLOAD_DIR}"
  URL "https://gstreamer.freedesktop.org/src/gst-plugins-bad/gst-plugins-bad-${RUFIN_GSTREAMER_VERSION}.tar.xz"
  URL_HASH "SHA256=${RUFIN_GSTREAMER_BAD_HASH}"
  BINARY_DIR "${RUFIN_EXTRA_AUDIO_BUILD_DIR}"
  CONFIGURE_COMMAND "${CMAKE_COMMAND}" -E env
    "CFLAGS=-I${RUFIN_GME_PREFIX}/include $ENV{CFLAGS}"
    "LDFLAGS=-L${RUFIN_GME_PREFIX}/lib $ENV{LDFLAGS}"
    "PKG_CONFIG_PATH=${RUFIN_OPENMPT_PREFIX}/lib/pkgconfig:$ENV{PKG_CONFIG_PATH}"
    "${RUFIN_MESON}" setup <BINARY_DIR> <SOURCE_DIR>
    -Dauto_features=disabled -Dgme=enabled -Dopenmpt=enabled --buildtype=release
  BUILD_COMMAND "${RUFIN_MESON}" compile -C <BINARY_DIR> gstgme gstopenmpt
  INSTALL_COMMAND ""
  BUILD_BYPRODUCTS "${RUFIN_GME_PLUGIN}" "${RUFIN_OPENMPT_PLUGIN}"
  USES_TERMINAL_DOWNLOAD TRUE
  USES_TERMINAL_BUILD TRUE
)

set(RUFIN_AUDIO_PLUGIN_DIR "${CMAKE_CURRENT_BINARY_DIR}/gst-plugins")
add_custom_target(rufin-audio-runtime
  COMMAND "${CMAKE_COMMAND}" -E make_directory "${RUFIN_AUDIO_PLUGIN_DIR}"
  COMMAND "${CMAKE_COMMAND}" -E copy_if_different
    "${RUFIN_WAVPACK_PLUGIN}" "${RUFIN_GME_PLUGIN}" "${RUFIN_OPENMPT_PLUGIN}"
    "${RUFIN_AUDIO_PLUGIN_DIR}"
  DEPENDS rufin-gst-wavpack rufin-gst-extra-audio
  VERBATIM
)
