if(NOT DEFINED ENV{MSYSTEM} OR NOT "$ENV{MSYSTEM}" STREQUAL "UCRT64")
  message(FATAL_ERROR "Rufin Windows builds require the MSYS2 UCRT64 environment")
endif()
if(NOT DEFINED ENV{MINGW_PREFIX} OR "$ENV{MINGW_PREFIX}" STREQUAL "")
  message(FATAL_ERROR "MINGW_PREFIX is not set by the MSYS2 UCRT64 environment")
endif()

find_program(RUFIN_MAKENSIS makensis REQUIRED)
find_program(RUFIN_GLIB_COMPILE_SCHEMAS glib-compile-schemas REQUIRED)
find_program(RUFIN_GTK_UPDATE_ICON_CACHE gtk4-update-icon-cache REQUIRED)
find_program(RUFIN_OBJDUMP NAMES llvm-objdump objdump REQUIRED)
pkg_get_variable(RUFIN_WINDOWS_GLIB_PREFIX glib-2.0 prefix)
pkg_get_variable(RUFIN_WINDOWS_GTK_PREFIX gtk4 prefix)
pkg_get_variable(RUFIN_WINDOWS_GSTREAMER_PREFIX gstreamer-1.0 prefix)
pkg_get_variable(RUFIN_WINDOWS_GSTREAMER_PLUGIN_DIR gstreamer-1.0 pluginsdir)
pkg_get_variable(RUFIN_WINDOWS_PIXBUF_PREFIX gdk-pixbuf-2.0 prefix)
pkg_get_variable(RUFIN_WINDOWS_PIXBUF_MODULE_DIR gdk-pixbuf-2.0 gdk_pixbuf_moduledir)
pkg_get_variable(RUFIN_WINDOWS_GIO_MODULE_DIR gio-2.0 giomoduledir)
cmake_path(GET RUFIN_WINDOWS_PIXBUF_MODULE_DIR PARENT_PATH RUFIN_WINDOWS_PIXBUF_ABI_DIR)
cmake_path(GET RUFIN_WINDOWS_PIXBUF_ABI_DIR PARENT_PATH RUFIN_WINDOWS_PIXBUF_ROOT)

if(RUFIN_BUILD_IDENTITY STREQUAL "development")
  set(RUFIN_WINDOWS_APP_ID io.github.screwys.Rufin.Devel)
  set(RUFIN_WINDOWS_DISPLAY_NAME "Rufin (Development)")
  set(RUFIN_WINDOWS_PROJECT_NAME Rufin.Devel)
else()
  set(RUFIN_WINDOWS_APP_ID io.github.screwys.Rufin)
  set(RUFIN_WINDOWS_DISPLAY_NAME Rufin)
  set(RUFIN_WINDOWS_PROJECT_NAME Rufin)
endif()

set(RUFIN_WINDOWS_PLUGIN_FILES)
set(RUFIN_WINDOWS_PLUGIN_STEMS
  ${RUFIN_GSTREAMER_COMMON_PLUGIN_STEMS}
  directsound
  gme
  openmpt
  wasapi
  wavpack
)
foreach(RUFIN_WINDOWS_PLUGIN_STEM IN LISTS RUFIN_WINDOWS_PLUGIN_STEMS)
  set(RUFIN_WINDOWS_PLUGIN_FILE
    "${RUFIN_WINDOWS_GSTREAMER_PLUGIN_DIR}/libgst${RUFIN_WINDOWS_PLUGIN_STEM}.dll")
  if(NOT EXISTS "${RUFIN_WINDOWS_PLUGIN_FILE}")
    message(FATAL_ERROR "Required GStreamer plugin is missing: ${RUFIN_WINDOWS_PLUGIN_FILE}")
  endif()
  list(APPEND RUFIN_WINDOWS_PLUGIN_FILES "${RUFIN_WINDOWS_PLUGIN_FILE}")
endforeach()

set(RUFIN_WINDOWS_GSPAWN_HELPERS
  "${RUFIN_WINDOWS_GLIB_PREFIX}/bin/gspawn-win64-helper.exe"
  "${RUFIN_WINDOWS_GLIB_PREFIX}/bin/gspawn-win64-helper-console.exe"
)
foreach(RUFIN_WINDOWS_REQUIRED_FILE IN LISTS RUFIN_WINDOWS_GSPAWN_HELPERS)
  if(NOT EXISTS "${RUFIN_WINDOWS_REQUIRED_FILE}")
    message(FATAL_ERROR "Required Windows runtime file is missing: ${RUFIN_WINDOWS_REQUIRED_FILE}")
  endif()
endforeach()

if(RUFIN_CARGO_FROZEN)
  set(RUFIN_WINDOWS_CARGO_LOCK_FLAG --frozen)
else()
  set(RUFIN_WINDOWS_CARGO_LOCK_FLAG --locked)
endif()
set(RUFIN_WINDOWS_UPDATER_PROFILE_ARGUMENTS)
if(RUFIN_RESOLVED_CARGO_PROFILE STREQUAL "release")
  list(APPEND RUFIN_WINDOWS_UPDATER_PROFILE_ARGUMENTS --release)
elseif(NOT RUFIN_RESOLVED_CARGO_PROFILE STREQUAL "debug")
  list(APPEND RUFIN_WINDOWS_UPDATER_PROFILE_ARGUMENTS
    --profile "${RUFIN_RESOLVED_CARGO_PROFILE}")
endif()
set(RUFIN_WINDOWS_UPDATER
  "${RUFIN_CARGO_TARGET_DIR}/${RUFIN_RESOLVED_CARGO_PROFILE}/rufin-update-helper.exe")
if(RUFIN_BUILD_IDENTITY STREQUAL "stable")
  add_custom_target(rufin-update-helper
    COMMAND "${CMAKE_COMMAND}" -E env
      "CARGO_TARGET_DIR=${RUFIN_CARGO_TARGET_DIR}"
      "CMAKE_GENERATOR=${CMAKE_GENERATOR}"
      "${RUFIN_CARGO}" build
      "${RUFIN_WINDOWS_CARGO_LOCK_FLAG}"
      --package windows-updater
      --bin rufin-update-helper
      ${RUFIN_WINDOWS_UPDATER_PROFILE_ARGUMENTS}
    BYPRODUCTS "${RUFIN_WINDOWS_UPDATER}"
    WORKING_DIRECTORY "${CMAKE_CURRENT_SOURCE_DIR}"
    USES_TERMINAL
    COMMAND_EXPAND_LISTS
    VERBATIM
  )
endif()

install(FILES LICENSE packaging/windows/assets/rufin.ico DESTINATION .)
install(FILES ${RUFIN_WINDOWS_GSPAWN_HELPERS} DESTINATION bin)
install(FILES ${RUFIN_WINDOWS_PLUGIN_FILES} DESTINATION lib/gstreamer-1.0)
install(DIRECTORY
  "${RUFIN_WINDOWS_PIXBUF_ROOT}/"
  DESTINATION lib/gdk-pixbuf-2.0
  PATTERN "*.dll.a" EXCLUDE)
install(DIRECTORY "${RUFIN_WINDOWS_GIO_MODULE_DIR}/" DESTINATION lib/gio/modules)
install(DIRECTORY "${RUFIN_WINDOWS_GSTREAMER_PREFIX}/libexec/gstreamer-1.0/"
  DESTINATION libexec/gstreamer-1.0)
install(DIRECTORY "${RUFIN_WINDOWS_GLIB_PREFIX}/share/glib-2.0/schemas/"
  DESTINATION share/glib-2.0/schemas)
install(DIRECTORY "${RUFIN_WINDOWS_GSTREAMER_PREFIX}/share/gstreamer-1.0/"
  DESTINATION share/gstreamer-1.0)
install(DIRECTORY "${RUFIN_WINDOWS_GTK_PREFIX}/share/gtk-4.0/" DESTINATION share/gtk-4.0)
install(DIRECTORY "${RUFIN_WINDOWS_GTK_PREFIX}/share/icons/Adwaita/"
  DESTINATION share/icons/Adwaita)
install(DIRECTORY "${RUFIN_WINDOWS_GTK_PREFIX}/share/icons/AdwaitaLegacy/"
  DESTINATION share/icons/AdwaitaLegacy)
install(DIRECTORY "${RUFIN_WINDOWS_GTK_PREFIX}/share/icons/hicolor/"
  DESTINATION share/icons/hicolor)
install(DIRECTORY "${CMAKE_CURRENT_SOURCE_DIR}/data/icons/hicolor/"
  DESTINATION share/icons/hicolor)
install(DIRECTORY "${RUFIN_WINDOWS_GTK_PREFIX}/share/mime/" DESTINATION share/mime)
install(DIRECTORY "${RUFIN_WINDOWS_GTK_PREFIX}/share/licenses/" DESTINATION share/licenses)
foreach(RUFIN_PO_FILE IN LISTS RUFIN_PO_FILES)
  get_filename_component(RUFIN_LOCALE "${RUFIN_PO_FILE}" NAME_WE)
  if(EXISTS "${RUFIN_WINDOWS_GLIB_PREFIX}/share/locale/${RUFIN_LOCALE}")
    install(DIRECTORY "${RUFIN_WINDOWS_GLIB_PREFIX}/share/locale/${RUFIN_LOCALE}/"
      DESTINATION "share/locale/${RUFIN_LOCALE}")
  endif()
endforeach()
file(GENERATE OUTPUT "${CMAKE_CURRENT_BINARY_DIR}/windows-settings.ini"
  CONTENT "[Settings]\ngtk-font-name=Segoe UI 9\n")
install(FILES "${CMAKE_CURRENT_BINARY_DIR}/windows-settings.ini"
  DESTINATION etc/gtk-4.0 RENAME settings.ini)

if(RUFIN_BUILD_IDENTITY STREQUAL "stable")
  file(GENERATE OUTPUT "${CMAKE_CURRENT_BINARY_DIR}/rufin-update-helper.complete"
    CONTENT "rufin-update-helper:${RUFIN_VERSION}\n")
  install(PROGRAMS "${RUFIN_WINDOWS_UPDATER}"
    DESTINATION "updater/${RUFIN_VERSION}")
  install(FILES "${CMAKE_CURRENT_BINARY_DIR}/rufin-update-helper.complete"
    DESTINATION "updater/${RUFIN_VERSION}")
endif()

set(RUFIN_WINDOWS_RUNTIME_DIRS
  "${RUFIN_WINDOWS_GLIB_PREFIX}/bin"
  "${RUFIN_WINDOWS_GTK_PREFIX}/bin"
  "${RUFIN_WINDOWS_GSTREAMER_PREFIX}/bin"
  "${RUFIN_WINDOWS_PIXBUF_PREFIX}/bin"
)
list(REMOVE_DUPLICATES RUFIN_WINDOWS_RUNTIME_DIRS)
set(RUFIN_WINDOWS_OBJDUMP "${RUFIN_OBJDUMP}")
set(RUFIN_WINDOWS_SCHEMA_COMMAND "${RUFIN_GLIB_COMPILE_SCHEMAS}")
set(RUFIN_WINDOWS_ICON_COMMAND "${RUFIN_GTK_UPDATE_ICON_CACHE}")
configure_file(
  "${CMAKE_CURRENT_LIST_DIR}/RufinWindowsInstall.cmake.in"
  "${CMAKE_CURRENT_BINARY_DIR}/RufinWindowsInstall.cmake"
  @ONLY
)
install(SCRIPT "${CMAKE_CURRENT_BINARY_DIR}/RufinWindowsInstall.cmake")

set(RUFIN_WINDOWS_STAGE_DIR
  "${CMAKE_CURRENT_BINARY_DIR}/${RUFIN_WINDOWS_PROJECT_NAME}")
add_custom_target(rufin-stage
  COMMAND "${CMAKE_COMMAND}" -E rm -rf "${RUFIN_WINDOWS_STAGE_DIR}"
  COMMAND "${CMAKE_COMMAND}" --install "${CMAKE_BINARY_DIR}"
    --prefix "${RUFIN_WINDOWS_STAGE_DIR}"
  DEPENDS rufin
  USES_TERMINAL
  VERBATIM
)
if(TARGET rufin-update-helper)
  add_dependencies(rufin-stage rufin-update-helper)
endif()

string(REGEX MATCH "^([0-9]+)\\.([0-9]+)\\.([0-9]+)" _ "${RUFIN_VERSION}")
set(RUFIN_WINDOWS_VERSION_QUAD
  "${CMAKE_MATCH_1}.${CMAKE_MATCH_2}.${CMAKE_MATCH_3}.0")
set(RUFIN_WINDOWS_INSTALLER
  "${RUFIN_ARTIFACT_DIR}/${RUFIN_WINDOWS_PROJECT_NAME}-${RUFIN_VERSION}-setup.exe")
add_custom_target(rufin-installer
  COMMAND "${CMAKE_COMMAND}" -E make_directory "${RUFIN_ARTIFACT_DIR}"
  COMMAND "${RUFIN_MAKENSIS}"
    "/DRUFIN_STAGE_DIR=${RUFIN_WINDOWS_STAGE_DIR}"
    "/DRUFIN_STAGE_FILES=${RUFIN_WINDOWS_STAGE_DIR}\\*"
    "/DRUFIN_OUTPUT_DIR=${RUFIN_ARTIFACT_DIR}"
    "/DRUFIN_ASSET_DIR=${CMAKE_CURRENT_SOURCE_DIR}/packaging/windows/assets"
    "/DRUFIN_APP_ID=${RUFIN_WINDOWS_APP_ID}"
    "/DRUFIN_DISPLAY_NAME=${RUFIN_WINDOWS_DISPLAY_NAME}"
    "/DRUFIN_PROJECT_NAME=${RUFIN_WINDOWS_PROJECT_NAME}"
    "/DRUFIN_VERSION=${RUFIN_VERSION}"
    "/DRUFIN_VERSION_QUAD=${RUFIN_WINDOWS_VERSION_QUAD}"
    "${CMAKE_CURRENT_SOURCE_DIR}/packaging/windows/rufin.nsi"
  DEPENDS rufin-stage
  BYPRODUCTS "${RUFIN_WINDOWS_INSTALLER}"
  USES_TERMINAL
  VERBATIM
)
