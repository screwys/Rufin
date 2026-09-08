foreach(RUFIN_DMG_ATTEMPT RANGE 1 3)
  # diskimages-helper can keep inherited output pipes open after hdiutil exits.
  execute_process(
    COMMAND "${RUFIN_HDIUTIL}" create
      -volname "${RUFIN_BUNDLE_NAME}"
      -srcfolder "${RUFIN_DMG_ROOT}"
      -ov -format UDZO "${RUFIN_DMG_ARTIFACT}"
    RESULT_VARIABLE RUFIN_DMG_STATUS
    OUTPUT_FILE "${RUFIN_DMG_ROOT}.log"
    ERROR_FILE "${RUFIN_DMG_ROOT}.log"
  )
  file(READ "${RUFIN_DMG_ROOT}.log" RUFIN_DMG_OUTPUT)
  message("${RUFIN_DMG_OUTPUT}")
  if(RUFIN_DMG_STATUS EQUAL 0)
    return()
  endif()
  if(NOT RUFIN_DMG_OUTPUT MATCHES "Resource busy" OR RUFIN_DMG_ATTEMPT EQUAL 3)
    message(FATAL_ERROR "hdiutil create failed with status ${RUFIN_DMG_STATUS}")
  endif()
  message(STATUS "hdiutil create reported Resource busy; retrying in 5 seconds (${RUFIN_DMG_ATTEMPT}/3)")
  execute_process(COMMAND "${CMAKE_COMMAND}" -E sleep 5)
endforeach()
