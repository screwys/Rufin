cmake_minimum_required(VERSION 3.25)

project(Rufin LANGUAGES NONE)

find_package(PkgConfig REQUIRED)
set(CMAKE_INSTALL_LIBDIR lib CACHE PATH "Library installation directory")
include(GNUInstallDirs)

set(RUFIN_BUILD_IDENTITY "stable" CACHE STRING "Rufin application identity")
set_property(CACHE RUFIN_BUILD_IDENTITY PROPERTY STRINGS stable development)
set(RUFIN_CARGO_PROFILE "auto" CACHE STRING "Cargo profile selected by CMake")
option(RUFIN_CARGO_FROZEN "Require Cargo's lockfile and cache to remain unchanged" OFF)
option(RUFIN_ENABLE_PACKAGING "Configure native package targets" OFF)
if(DEFINED ENV{RUFIN_ARTIFACT_ROOT} AND NOT "$ENV{RUFIN_ARTIFACT_ROOT}" STREQUAL "")
  set(RUFIN_ARTIFACT_DIR "$ENV{RUFIN_ARTIFACT_ROOT}" CACHE PATH "Package output directory")
else()
  set(RUFIN_ARTIFACT_DIR "${CMAKE_CURRENT_SOURCE_DIR}/.local/artifacts"
    CACHE PATH "Package output directory")
endif()
option(
  RUFIN_VALIDATE_DEVELOPMENT_RUNTIME
  "Require the GStreamer elements needed to run Rufin in this environment"
  OFF
)

if(NOT RUFIN_BUILD_IDENTITY STREQUAL "stable"
   AND NOT RUFIN_BUILD_IDENTITY STREQUAL "development")
  message(FATAL_ERROR "RUFIN_BUILD_IDENTITY must be stable or development")
endif()
if(RUFIN_BUILD_IDENTITY STREQUAL "development")
  set(RUFIN_APP_ID io.github.screwys.Rufin.Devel)
  set(RUFIN_DESKTOP_FILE data/io.github.screwys.Rufin.Devel.desktop)
else()
  set(RUFIN_APP_ID io.github.screwys.Rufin)
  set(RUFIN_DESKTOP_FILE data/io.github.screwys.Rufin.desktop)
endif()
set(RUFIN_GSTREAMER_COMMON_PLUGIN_STEMS
  aiff
  alaw
  apetag
  asf
  audioconvert
  audiofx
  audioparsers
  audioresample
  autodetect
  coreelements
  equalizer
  fdkaac
  flac
  id3demux
  isomp4
  level
  libav
  matroska
  mpg123
  musepack
  mulaw
  ogg
  opus
  opusparse
  pbtypes
  playback
  replaygain
  soup
  speex
  typefindfunctions
  volume
  vorbis
  wavparse
)

if(NOT CMAKE_BUILD_TYPE)
  set(CMAKE_BUILD_TYPE Debug CACHE STRING "Build type" FORCE)
endif()
if(APPLE)
  if(DEFINED ENV{MACOSX_DEPLOYMENT_TARGET}
     AND NOT "$ENV{MACOSX_DEPLOYMENT_TARGET}" STREQUAL "")
    set(RUFIN_MACOS_DEPLOYMENT_TARGET "$ENV{MACOSX_DEPLOYMENT_TARGET}")
  else()
    set(RUFIN_MACOS_DEPLOYMENT_TARGET 15.0)
  endif()
endif()

file(STRINGS "${CMAKE_CURRENT_SOURCE_DIR}/Cargo.toml" RUFIN_VERSION_LINE
  REGEX [=[^version = "[^"]+"$]=] LIMIT_COUNT 1)
if(NOT RUFIN_VERSION_LINE)
  message(FATAL_ERROR "The Rufin workspace version is missing from Cargo.toml")
endif()
string(REGEX REPLACE [=[^version = "([^"]+)".*]=] "\\1" RUFIN_VERSION
  "${RUFIN_VERSION_LINE}")

find_program(RUFIN_CARGO cargo REQUIRED)
find_program(RUFIN_RUSTC rustc REQUIRED)
find_program(RUFIN_MSGFMT msgfmt REQUIRED)

execute_process(
  COMMAND "${RUFIN_RUSTC}" --version
  RESULT_VARIABLE RUFIN_RUSTC_STATUS
  OUTPUT_VARIABLE RUFIN_RUSTC_VERSION_OUTPUT
  OUTPUT_STRIP_TRAILING_WHITESPACE
)
if(NOT RUFIN_RUSTC_STATUS EQUAL 0)
  message(FATAL_ERROR "rustc --version failed")
endif()
string(REGEX MATCH "rustc ([0-9]+\\.[0-9]+\\.[0-9]+)" _ "${RUFIN_RUSTC_VERSION_OUTPUT}")
set(RUFIN_RUSTC_VERSION "${CMAKE_MATCH_1}")
if(NOT RUFIN_RUSTC_VERSION OR RUFIN_RUSTC_VERSION VERSION_LESS 1.95)
  message(FATAL_ERROR "Rufin requires Rust 1.95 or newer; found ${RUFIN_RUSTC_VERSION_OUTPUT}")
endif()

pkg_check_modules(RUFIN_GLIB REQUIRED IMPORTED_TARGET glib-2.0)
pkg_check_modules(RUFIN_GTK REQUIRED IMPORTED_TARGET gtk4>=4.20)
pkg_check_modules(RUFIN_ADWAITA REQUIRED IMPORTED_TARGET libadwaita-1>=1.9)
pkg_check_modules(RUFIN_GSTREAMER REQUIRED IMPORTED_TARGET
  gstreamer-1.0>=1.26
  gstreamer-app-1.0>=1.26
  gstreamer-audio-1.0>=1.26
  gstreamer-pbutils-1.0>=1.26
)

if(RUFIN_VALIDATE_DEVELOPMENT_RUNTIME)
  find_program(RUFIN_GST_INSPECT gst-inspect-1.0 REQUIRED)
  execute_process(
    COMMAND "${RUFIN_GST_INSPECT}" --exists playbin3
    RESULT_VARIABLE RUFIN_PLAYBIN3_STATUS
  )
  execute_process(
    COMMAND "${RUFIN_GST_INSPECT}" --exists playbin
    RESULT_VARIABLE RUFIN_PLAYBIN_STATUS
  )
  if(NOT RUFIN_PLAYBIN3_STATUS EQUAL 0 AND NOT RUFIN_PLAYBIN_STATUS EQUAL 0)
    message(FATAL_ERROR "GStreamer playbin3 or playbin is required to run Rufin")
  endif()

  if(APPLE)
    set(RUFIN_AUDIO_SINK osxaudiosink)
  else()
    set(RUFIN_AUDIO_SINK autoaudiosink)
  endif()
  set(RUFIN_REQUIRED_GSTREAMER_ELEMENTS
    audioconvert
    audioresample
    "${RUFIN_AUDIO_SINK}"
    equalizer-nbands
    scaletempo
    souphttpsrc
  )
  foreach(RUFIN_GSTREAMER_ELEMENT IN LISTS RUFIN_REQUIRED_GSTREAMER_ELEMENTS)
    execute_process(
      COMMAND "${RUFIN_GST_INSPECT}" --exists "${RUFIN_GSTREAMER_ELEMENT}"
      RESULT_VARIABLE RUFIN_GSTREAMER_ELEMENT_STATUS
    )
    if(NOT RUFIN_GSTREAMER_ELEMENT_STATUS EQUAL 0)
      message(FATAL_ERROR
        "GStreamer element ${RUFIN_GSTREAMER_ELEMENT} is required to run Rufin")
    endif()
  endforeach()
endif()

include(cmake/RufinCargo.cmake)

if(APPLE)
  if(RUFIN_BUILD_IDENTITY STREQUAL "development")
    set(RUFIN_BUNDLE_NAME Rufin.Devel)
  else()
    set(RUFIN_BUNDLE_NAME Rufin)
  endif()
  set(RUFIN_INSTALL_BINDIR "${RUFIN_BUNDLE_NAME}.app/Contents/MacOS")
  set(RUFIN_INSTALL_DATADIR "${RUFIN_BUNDLE_NAME}.app/Contents/Resources/share")
  set(RUFIN_INSTALL_LOCALEDIR "${RUFIN_INSTALL_DATADIR}/locale")
else()
  set(RUFIN_INSTALL_BINDIR "${CMAKE_INSTALL_BINDIR}")
  set(RUFIN_INSTALL_DATADIR "${CMAKE_INSTALL_DATADIR}")
  set(RUFIN_INSTALL_LOCALEDIR "${CMAKE_INSTALL_LOCALEDIR}")
endif()

file(GLOB RUFIN_PO_FILES CONFIGURE_DEPENDS "${CMAKE_CURRENT_SOURCE_DIR}/locales/*.po")
set(RUFIN_COMPILED_LOCALES)
foreach(RUFIN_PO_FILE IN LISTS RUFIN_PO_FILES)
  get_filename_component(RUFIN_LOCALE "${RUFIN_PO_FILE}" NAME_WE)
  set(RUFIN_MO_FILE
    "${CMAKE_CURRENT_BINARY_DIR}/locale/${RUFIN_LOCALE}/LC_MESSAGES/rufin.mo")
  add_custom_command(
    OUTPUT "${RUFIN_MO_FILE}"
    COMMAND "${CMAKE_COMMAND}" -E make_directory
      "${CMAKE_CURRENT_BINARY_DIR}/locale/${RUFIN_LOCALE}/LC_MESSAGES"
    COMMAND "${RUFIN_MSGFMT}" --check "${RUFIN_PO_FILE}" -o "${RUFIN_MO_FILE}"
    DEPENDS "${RUFIN_PO_FILE}"
    VERBATIM
  )
  list(APPEND RUFIN_COMPILED_LOCALES "${RUFIN_MO_FILE}")
  install(FILES "${RUFIN_MO_FILE}"
    DESTINATION "${RUFIN_INSTALL_LOCALEDIR}/${RUFIN_LOCALE}/LC_MESSAGES")
endforeach()
add_custom_target(rufin-translations ALL DEPENDS ${RUFIN_COMPILED_LOCALES})
add_dependencies(rufin rufin-translations)

install(PROGRAMS "${RUFIN_CMAKE_EXECUTABLE}"
  DESTINATION "${RUFIN_INSTALL_BINDIR}"
  RENAME "rufin${CMAKE_EXECUTABLE_SUFFIX}")
install(FILES
  data/japanese-readings.dic
  DESTINATION "${RUFIN_INSTALL_DATADIR}/rufin")
install(FILES
  data/japanese-readings.LICENSE
  DESTINATION "${RUFIN_INSTALL_DATADIR}/licenses/rufin")
if(UNIX AND NOT APPLE)
  install(FILES
    "${RUFIN_DESKTOP_FILE}"
    DESTINATION "${RUFIN_INSTALL_DATADIR}/applications"
    RENAME "${RUFIN_APP_ID}.desktop")
  install(FILES
    data/io.github.screwys.Rufin.metainfo.xml
    DESTINATION "${RUFIN_INSTALL_DATADIR}/metainfo")
  install(DIRECTORY data/icons/hicolor/
    DESTINATION "${RUFIN_INSTALL_DATADIR}/icons/hicolor")
endif()

if(WIN32 AND RUFIN_ENABLE_PACKAGING)
  include(cmake/RufinWindows.cmake)
elseif(APPLE AND RUFIN_ENABLE_PACKAGING)
  include(cmake/RufinMacOS.cmake)
endif()

message(STATUS "Rufin ${RUFIN_VERSION}")
message(STATUS "Application identity: ${RUFIN_BUILD_IDENTITY}")
message(STATUS "Cargo profile: ${RUFIN_RESOLVED_CARGO_PROFILE}")
message(STATUS "Cargo target directory: ${RUFIN_CARGO_TARGET_DIR}")
message(STATUS "Artifact directory: ${RUFIN_ARTIFACT_DIR}")
