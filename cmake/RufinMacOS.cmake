include(ExternalProject)

find_program(RUFIN_BREW brew REQUIRED)
find_program(RUFIN_CODESIGN codesign REQUIRED)
find_program(RUFIN_DITTO ditto REQUIRED)
find_program(RUFIN_FILE file REQUIRED)
find_program(RUFIN_GIO_QUERYMODULES gio-querymodules REQUIRED)
find_program(RUFIN_GLIB_COMPILE_SCHEMAS glib-compile-schemas REQUIRED)
find_program(RUFIN_HDIUTIL hdiutil REQUIRED)
find_program(RUFIN_ICONUTIL iconutil REQUIRED)
find_program(RUFIN_INSTALL_NAME_TOOL install_name_tool REQUIRED)
find_program(RUFIN_LIPO lipo REQUIRED)
find_program(RUFIN_MESON meson REQUIRED)
find_program(RUFIN_RSVG_CONVERT rsvg-convert REQUIRED)

function(rufin_macos_install_file source destination)
  get_filename_component(RUFIN_COPY_DESTINATION_DIR "${destination}" DIRECTORY)
  set(RUFIN_COPY_SOURCE "${source}")
  set(RUFIN_COPY_DESTINATION "${destination}")
  set(RUFIN_COPY_CODE [=[
file(MAKE_DIRECTORY
  "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/@RUFIN_COPY_DESTINATION_DIR@")
execute_process(
  COMMAND "@CMAKE_COMMAND@" -E copy_if_different
    "@RUFIN_COPY_SOURCE@"
    "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/@RUFIN_COPY_DESTINATION@"
  COMMAND_ERROR_IS_FATAL ANY
)
]=])
  string(CONFIGURE "${RUFIN_COPY_CODE}" RUFIN_COPY_CODE @ONLY)
  install(CODE "${RUFIN_COPY_CODE}")
endfunction()

function(rufin_macos_install_directory source destination)
  if(NOT IS_DIRECTORY "${source}")
    return()
  endif()
  set(RUFIN_COPY_SOURCE "${source}")
  set(RUFIN_COPY_DESTINATION "${destination}")
  set(RUFIN_COPY_CODE [=[
file(MAKE_DIRECTORY
  "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/@RUFIN_COPY_DESTINATION@")
execute_process(
  COMMAND "@CMAKE_COMMAND@" -E copy_directory
    "@RUFIN_COPY_SOURCE@"
    "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/@RUFIN_COPY_DESTINATION@"
  COMMAND_ERROR_IS_FATAL ANY
)
]=])
  string(CONFIGURE "${RUFIN_COPY_CODE}" RUFIN_COPY_CODE @ONLY)
  install(CODE "${RUFIN_COPY_CODE}")
endfunction()

pkg_check_modules(RUFIN_PIXBUF REQUIRED gdk-pixbuf-2.0)
pkg_check_modules(RUFIN_SOUP REQUIRED libsoup-3.0)
pkg_get_variable(RUFIN_GSTREAMER_PLUGIN_DIR gstreamer-1.0 pluginsdir)
pkg_get_variable(RUFIN_GSTREAMER_SCANNER_DIR gstreamer-1.0 pluginscannerdir)
pkg_get_variable(RUFIN_PIXBUF_MODULE_DIR gdk-pixbuf-2.0 gdk_pixbuf_moduledir)
pkg_get_variable(RUFIN_PIXBUF_QUERY_LOADERS gdk-pixbuf-2.0 gdk_pixbuf_query_loaders)

execute_process(
  COMMAND "${RUFIN_BREW}" --prefix
  OUTPUT_VARIABLE RUFIN_BREW_PREFIX
  OUTPUT_STRIP_TRAILING_WHITESPACE
  COMMAND_ERROR_IS_FATAL ANY
)
execute_process(
  COMMAND "${RUFIN_BREW}" --prefix game-music-emu
  OUTPUT_VARIABLE RUFIN_GME_PREFIX
  OUTPUT_STRIP_TRAILING_WHITESPACE
  COMMAND_ERROR_IS_FATAL ANY
)
execute_process(
  COMMAND "${RUFIN_BREW}" --prefix libopenmpt
  OUTPUT_VARIABLE RUFIN_OPENMPT_PREFIX
  OUTPUT_STRIP_TRAILING_WHITESPACE
  COMMAND_ERROR_IS_FATAL ANY
)

if(RUFIN_BUILD_IDENTITY STREQUAL "development")
  set(RUFIN_MACOS_DISPLAY_NAME "Rufin (Development)")
  set(RUFIN_MACOS_SIGN_IDENTITY "$ENV{RUFIN_MACOS_SIGN_IDENTITY}")
  if(RUFIN_MACOS_SIGN_IDENTITY STREQUAL "")
    set(RUFIN_MACOS_SIGN_IDENTITY "Rufin Development")
  endif()
else()
  set(RUFIN_MACOS_DISPLAY_NAME Rufin)
  set(RUFIN_MACOS_SIGN_IDENTITY "$ENV{RUFIN_MACOS_SIGN_IDENTITY}")
  if(RUFIN_MACOS_SIGN_IDENTITY STREQUAL "" OR RUFIN_MACOS_SIGN_IDENTITY STREQUAL "-")
    message(FATAL_ERROR "Stable macOS packages require RUFIN_MACOS_SIGN_IDENTITY")
  endif()
endif()
set(RUFIN_MACOS_SIGN_KEYCHAIN "$ENV{RUFIN_MACOS_SIGN_KEYCHAIN}")

set(RUFIN_MACOS_PLUGIN_FILES)
set(RUFIN_MACOS_PLUGIN_STEMS ${RUFIN_GSTREAMER_COMMON_PLUGIN_STEMS} osxaudio)
foreach(RUFIN_MACOS_PLUGIN_STEM IN LISTS RUFIN_MACOS_PLUGIN_STEMS)
  set(RUFIN_MACOS_PLUGIN_FILE
    "${RUFIN_GSTREAMER_PLUGIN_DIR}/libgst${RUFIN_MACOS_PLUGIN_STEM}.dylib")
  if(NOT EXISTS "${RUFIN_MACOS_PLUGIN_FILE}")
    message(FATAL_ERROR "Required GStreamer plugin is missing: ${RUFIN_MACOS_PLUGIN_FILE}")
  endif()
  list(APPEND RUFIN_MACOS_PLUGIN_FILES "${RUFIN_MACOS_PLUGIN_FILE}")
endforeach()

set(RUFIN_MACOS_DOWNLOAD_DIR "${CMAKE_CURRENT_BINARY_DIR}/downloads")
file(MAKE_DIRECTORY "${RUFIN_MACOS_DOWNLOAD_DIR}")
execute_process(
  COMMAND "${PKG_CONFIG_EXECUTABLE}" --modversion gstreamer-1.0
  OUTPUT_VARIABLE RUFIN_GSTREAMER_VERSION
  OUTPUT_STRIP_TRAILING_WHITESPACE
  COMMAND_ERROR_IS_FATAL ANY
)

function(rufin_gstreamer_archive_hash component output_variable)
  set(RUFIN_CHECKSUM_FILE
    "${RUFIN_MACOS_DOWNLOAD_DIR}/gst-plugins-${component}-${RUFIN_GSTREAMER_VERSION}.sha256sum")
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
  DOWNLOAD_DIR "${RUFIN_MACOS_DOWNLOAD_DIR}"
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
  DOWNLOAD_DIR "${RUFIN_MACOS_DOWNLOAD_DIR}"
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

set(VERSION "${RUFIN_VERSION}")
set(MINIMUM_SYSTEM_VERSION "${RUFIN_MACOS_DEPLOYMENT_TARGET}")
set(BUNDLE_IDENTIFIER "${RUFIN_APP_ID}")
set(BUNDLE_NAME "${RUFIN_BUNDLE_NAME}")
set(DISPLAY_NAME "${RUFIN_MACOS_DISPLAY_NAME}")
configure_file(
  "${CMAKE_CURRENT_SOURCE_DIR}/packaging/macos/Info.plist.in"
  "${CMAKE_CURRENT_BINARY_DIR}/Info.plist"
  @ONLY
)

set(RUFIN_MACOS_ICONSET "${CMAKE_CURRENT_BINARY_DIR}/Rufin.iconset")
set(RUFIN_MACOS_ICON "${CMAKE_CURRENT_BINARY_DIR}/Rufin.icns")
set(RUFIN_MACOS_ICON_COMMANDS
  COMMAND "${CMAKE_COMMAND}" -E rm -rf "${RUFIN_MACOS_ICONSET}"
  COMMAND "${CMAKE_COMMAND}" -E make_directory "${RUFIN_MACOS_ICONSET}"
)
foreach(RUFIN_ICON_SIZE 16 32 128 256 512)
  math(EXPR RUFIN_ICON_DOUBLE_SIZE "${RUFIN_ICON_SIZE} * 2")
  list(APPEND RUFIN_MACOS_ICON_COMMANDS
    COMMAND "${RUFIN_RSVG_CONVERT}"
      -w "${RUFIN_ICON_SIZE}" -h "${RUFIN_ICON_SIZE}"
      -o "${RUFIN_MACOS_ICONSET}/icon_${RUFIN_ICON_SIZE}x${RUFIN_ICON_SIZE}.png"
      "${CMAKE_CURRENT_SOURCE_DIR}/data/icons/hicolor/scalable/apps/io.github.screwys.Rufin.svg"
    COMMAND "${RUFIN_RSVG_CONVERT}"
      -w "${RUFIN_ICON_DOUBLE_SIZE}" -h "${RUFIN_ICON_DOUBLE_SIZE}"
      -o "${RUFIN_MACOS_ICONSET}/icon_${RUFIN_ICON_SIZE}x${RUFIN_ICON_SIZE}@2x.png"
      "${CMAKE_CURRENT_SOURCE_DIR}/data/icons/hicolor/scalable/apps/io.github.screwys.Rufin.svg"
  )
endforeach()
add_custom_command(
  OUTPUT "${RUFIN_MACOS_ICON}"
  ${RUFIN_MACOS_ICON_COMMANDS}
  COMMAND "${RUFIN_ICONUTIL}" -c icns "${RUFIN_MACOS_ICONSET}" -o "${RUFIN_MACOS_ICON}"
  DEPENDS data/icons/hicolor/scalable/apps/io.github.screwys.Rufin.svg
  VERBATIM
)
add_custom_target(rufin-macos-icon DEPENDS "${RUFIN_MACOS_ICON}")

set(RUFIN_MACOS_CONTENTS "${RUFIN_BUNDLE_NAME}.app/Contents")
install(FILES "${CMAKE_CURRENT_BINARY_DIR}/Info.plist" DESTINATION "${RUFIN_MACOS_CONTENTS}")
install(FILES "${RUFIN_MACOS_ICON}"
  DESTINATION "${RUFIN_MACOS_CONTENTS}/Resources")
install(FILES LICENSE DESTINATION "${RUFIN_MACOS_CONTENTS}/Resources")
foreach(RUFIN_MACOS_EXECUTABLE
  "${RUFIN_GSTREAMER_SCANNER_DIR}/gst-plugin-scanner"
  "${RUFIN_PIXBUF_QUERY_LOADERS}")
  get_filename_component(RUFIN_MACOS_EXECUTABLE_NAME "${RUFIN_MACOS_EXECUTABLE}" NAME)
  rufin_macos_install_file("${RUFIN_MACOS_EXECUTABLE}"
    "${RUFIN_MACOS_CONTENTS}/MacOS/${RUFIN_MACOS_EXECUTABLE_NAME}")
endforeach()
foreach(RUFIN_MACOS_PLUGIN
  ${RUFIN_MACOS_PLUGIN_FILES}
  "${RUFIN_WAVPACK_PLUGIN}"
  "${RUFIN_GME_PLUGIN}"
  "${RUFIN_OPENMPT_PLUGIN}")
  get_filename_component(RUFIN_MACOS_PLUGIN_NAME "${RUFIN_MACOS_PLUGIN}" NAME)
  rufin_macos_install_file("${RUFIN_MACOS_PLUGIN}"
    "${RUFIN_MACOS_CONTENTS}/Resources/lib/gstreamer-1.0/${RUFIN_MACOS_PLUGIN_NAME}")
endforeach()
rufin_macos_install_directory("${RUFIN_PIXBUF_MODULE_DIR}"
  "${RUFIN_MACOS_CONTENTS}/Resources/lib/gdk-pixbuf-2.0/loaders")
foreach(RUFIN_MACOS_RUNTIME_DIRECTORY
  "lib/gio/modules"
  "share/glib-2.0/schemas"
  "share/gstreamer-1.0"
  "share/gtk-4.0"
  "share/icons/Adwaita"
  "share/icons/AdwaitaLegacy"
  "share/icons/hicolor"
  "share/mime")
  rufin_macos_install_directory(
    "${RUFIN_BREW_PREFIX}/${RUFIN_MACOS_RUNTIME_DIRECTORY}"
    "${RUFIN_MACOS_CONTENTS}/Resources/${RUFIN_MACOS_RUNTIME_DIRECTORY}")
endforeach()
foreach(RUFIN_PO_FILE IN LISTS RUFIN_PO_FILES)
  get_filename_component(RUFIN_LOCALE "${RUFIN_PO_FILE}" NAME_WE)
  rufin_macos_install_directory(
    "${RUFIN_BREW_PREFIX}/share/locale/${RUFIN_LOCALE}"
    "${RUFIN_MACOS_CONTENTS}/Resources/share/locale/${RUFIN_LOCALE}")
endforeach()

set(RUFIN_MACOS_BREW_PREFIX "${RUFIN_BREW_PREFIX}")
set(RUFIN_MACOS_GIO_QUERYMODULES "${RUFIN_GIO_QUERYMODULES}")
set(RUFIN_MACOS_GLIB_COMPILE_SCHEMAS "${RUFIN_GLIB_COMPILE_SCHEMAS}")
set(RUFIN_MACOS_CODESIGN "${RUFIN_CODESIGN}")
set(RUFIN_MACOS_FILE "${RUFIN_FILE}")
set(RUFIN_MACOS_INSTALL_NAME_TOOL "${RUFIN_INSTALL_NAME_TOOL}")
set(RUFIN_MACOS_LIPO "${RUFIN_LIPO}")
configure_file(
  "${CMAKE_CURRENT_LIST_DIR}/RufinMacOSInstall.cmake.in"
  "${CMAKE_CURRENT_BINARY_DIR}/RufinMacOSInstall.cmake"
  @ONLY
)
install(SCRIPT "${CMAKE_CURRENT_BINARY_DIR}/RufinMacOSInstall.cmake")

set(RUFIN_MACOS_STAGE "${CMAKE_CURRENT_BINARY_DIR}/stage")
set(RUFIN_MACOS_APP "${RUFIN_MACOS_STAGE}/${RUFIN_BUNDLE_NAME}.app")
add_custom_target(rufin-bundle
  COMMAND "${CMAKE_COMMAND}" -E rm -rf "${RUFIN_MACOS_STAGE}"
  COMMAND "${CMAKE_COMMAND}" --install "${CMAKE_BINARY_DIR}"
    --prefix "${RUFIN_MACOS_STAGE}"
  DEPENDS
    rufin
    rufin-gst-extra-audio
    rufin-gst-wavpack
    rufin-macos-icon
  USES_TERMINAL
  VERBATIM
)

if(DEFINED ENV{RUFIN_DMG_ARTIFACT} AND NOT "$ENV{RUFIN_DMG_ARTIFACT}" STREQUAL "")
  set(RUFIN_DMG_ARTIFACT "$ENV{RUFIN_DMG_ARTIFACT}")
else()
  set(RUFIN_DMG_ARTIFACT "${RUFIN_ARTIFACT_DIR}/${RUFIN_BUNDLE_NAME}.dmg")
endif()
set(RUFIN_DMG_ROOT "${CMAKE_CURRENT_BINARY_DIR}/dmg")
add_custom_target(rufin-dmg
  COMMAND "${CMAKE_COMMAND}" -E make_directory "${RUFIN_ARTIFACT_DIR}"
  COMMAND "${CMAKE_COMMAND}" -E rm -rf "${RUFIN_DMG_ROOT}"
  COMMAND "${CMAKE_COMMAND}" -E make_directory "${RUFIN_DMG_ROOT}"
  COMMAND "${RUFIN_DITTO}" "${RUFIN_MACOS_APP}"
    "${RUFIN_DMG_ROOT}/${RUFIN_BUNDLE_NAME}.app"
  COMMAND "${CMAKE_COMMAND}" -E create_symlink /Applications
    "${RUFIN_DMG_ROOT}/Applications"
  COMMAND "${RUFIN_HDIUTIL}" create
    -volname "${RUFIN_BUNDLE_NAME}"
    -srcfolder "${RUFIN_DMG_ROOT}"
    -ov -format UDZO "${RUFIN_DMG_ARTIFACT}"
  DEPENDS rufin-bundle
  BYPRODUCTS "${RUFIN_DMG_ARTIFACT}"
  USES_TERMINAL
  VERBATIM
)
add_custom_target(rufin-native-package DEPENDS rufin-dmg)
