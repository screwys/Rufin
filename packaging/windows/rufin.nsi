!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "LogicLib.nsh"
!include "nsDialogs.nsh"
!include "Win\COM.nsh"
!include "Win\Propkey.nsh"

!ifndef RUFIN_STAGE_DIR
!define RUFIN_STAGE_DIR "..\..\dist\windows\Rufin"
!endif

!ifndef RUFIN_STAGE_FILES
!define RUFIN_STAGE_FILES "${RUFIN_STAGE_DIR}\*"
!endif

!ifndef RUFIN_OUTPUT_DIR
!define RUFIN_OUTPUT_DIR "..\..\dist"
!endif

!ifndef RUFIN_ASSET_DIR
!define RUFIN_ASSET_DIR "assets"
!endif

!ifndef RUFIN_VERSION
!define RUFIN_VERSION "0.0.0"
!endif

!ifndef RUFIN_VERSION_QUAD
!define RUFIN_VERSION_QUAD "0.0.0.0"
!endif

!ifndef RUFIN_APP_ID
!define RUFIN_APP_ID "io.github.screwys.Rufin"
!endif

!ifndef RUFIN_DISPLAY_NAME
!define RUFIN_DISPLAY_NAME "Rufin"
!endif

!ifndef RUFIN_PROJECT_NAME
!define RUFIN_PROJECT_NAME "Rufin"
!endif

Unicode true
Name "${RUFIN_DISPLAY_NAME}"
OutFile "${RUFIN_OUTPUT_DIR}/${RUFIN_PROJECT_NAME}-${RUFIN_VERSION}-setup.exe"
InstallDir "$LOCALAPPDATA\Programs\${RUFIN_PROJECT_NAME}"
RequestExecutionLevel user
SetCompressor /SOLID lzma
Icon "${RUFIN_ASSET_DIR}/rufin.ico"
UninstallIcon "${RUFIN_ASSET_DIR}/rufin.ico"

VIProductVersion "${RUFIN_VERSION_QUAD}"
VIAddVersionKey /LANG=1033 "ProductName" "${RUFIN_DISPLAY_NAME}"
VIAddVersionKey /LANG=1033 "CompanyName" "screwy"
VIAddVersionKey /LANG=1033 "FileDescription" "${RUFIN_DISPLAY_NAME} installer"
VIAddVersionKey /LANG=1033 "FileVersion" "${RUFIN_VERSION}"
VIAddVersionKey /LANG=1033 "ProductVersion" "${RUFIN_VERSION}"
VIAddVersionKey /LANG=1033 "LegalCopyright" "GPL-3.0-or-later"

!define MUI_ABORTWARNING
!define MUI_ICON "${RUFIN_ASSET_DIR}/rufin.ico"
!define MUI_UNICON "${RUFIN_ASSET_DIR}/rufin.ico"
!define MUI_WELCOMEPAGE_TITLE "Welcome to ${RUFIN_DISPLAY_NAME}"
!define MUI_WELCOMEPAGE_TEXT " This will install ${RUFIN_DISPLAY_NAME} on your computer."
!define MUI_FINISHPAGE_RUN "$INSTDIR\bin\rufin.exe"

Var LegacyInstallDir
Var LegacyInstallOwned
Var InstallChannel
Var PurgeCache
Var PurgeCacheCheckbox

!macro CreateRufinShortcut SHORTCUT_PATH TARGET_PATH ICON_PATH
    !insertmacro ComHlpr_CreateInProcInstance ${CLSID_ShellLink} ${IID_IShellLink} r0 ""
    ${If} $0 P<> 0
        ${IShellLink::SetPath} $0 '("${TARGET_PATH}").r1'
        ${IShellLink::SetWorkingDirectory} $0 '("$INSTDIR").r2'
        ${IShellLink::SetIconLocation} $0 '("${ICON_PATH}", 0).r3'
        ${If} $1 = 0
        ${AndIf} $2 = 0
        ${AndIf} $3 = 0
            ${IUnknown::QueryInterface} $0 '("${IID_IPropertyStore}",.r1)'
            ${If} $1 P<> 0
                System::Call "oleaut32::SysAllocString(w '${RUFIN_APP_ID}') p .r4"
                System::Call '*${SYSSTRUCT_PROPERTYKEY}(${PKEY_AppUserModel_ID})p.r2'
                System::Call '*${SYSSTRUCT_PROPVARIANT}(${VT_BSTR},, p r4)p.r3'
                ${IPropertyStore::SetValue} $1 '($2, $3)'
                ${IPropertyStore::Commit} $1 ""
                System::Call "oleaut32::SysFreeString(p r4)"
                System::Free $2
                System::Free $3
                ${IUnknown::Release} $1 ""
            ${EndIf}
            ${IUnknown::QueryInterface} $0 '("${IID_IPersistFile}",.r1)'
            ${If} $1 P<> 0
                ${IPersistFile::Save} $1 '("${SHORTCUT_PATH}", 1)'
                ${IUnknown::Release} $1 ""
            ${EndIf}
        ${EndIf}
        ${IUnknown::Release} $0 ""
    ${EndIf}
!macroend

!macro RequireRufinClosed EXECUTABLE LABEL RUNNING_LABEL
    IfFileExists "${EXECUTABLE}" 0 ${LABEL}_not_running
    Delete "${EXECUTABLE}.rufin-install"
    ClearErrors
    Rename "${EXECUTABLE}" "${EXECUTABLE}.rufin-install"
    IfErrors ${RUNNING_LABEL}
    Rename "${EXECUTABLE}.rufin-install" "${EXECUTABLE}"
    IfErrors ${RUNNING_LABEL}

${LABEL}_not_running:
!macroend

!macro WriteRufinUninstallRegistration
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${RUFIN_PROJECT_NAME}" \
        "DisplayName" "${RUFIN_DISPLAY_NAME}"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${RUFIN_PROJECT_NAME}" \
        "DisplayVersion" "${RUFIN_VERSION}"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${RUFIN_PROJECT_NAME}" \
        "DisplayIcon" "$INSTDIR\rufin.ico"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${RUFIN_PROJECT_NAME}" \
        "Publisher" "screwy"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${RUFIN_PROJECT_NAME}" \
        "URLInfoAbout" "https://github.com/screwys/Rufin"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${RUFIN_PROJECT_NAME}" \
        "UninstallString" '$\"$INSTDIR\Uninstall.exe$\"'
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${RUFIN_PROJECT_NAME}" \
        "QuietUninstallString" '$\"$INSTDIR\Uninstall.exe$\" /S'
    WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${RUFIN_PROJECT_NAME}" \
        "NoModify" 1
    WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${RUFIN_PROJECT_NAME}" \
        "NoRepair" 1
!macroend

Function .onInit
    StrCpy $INSTDIR "$LOCALAPPDATA\Programs\${RUFIN_PROJECT_NAME}"
    StrCpy $LegacyInstallOwned 0
    StrCpy $InstallChannel "direct"
    ${GetParameters} $0
    ClearErrors
    ${GetOptions} $0 "/RUFINCHANNEL=" $1
    IfErrors update_channel_done
    StrCpy $InstallChannel $1
    StrCmp $InstallChannel "direct" update_channel_done
    StrCmp $InstallChannel "scoop" update_channel_done
    StrCmp $InstallChannel "winget" update_channel_done
    IfSilent invalid_channel_silent invalid_channel_message

invalid_channel_message:
    MessageBox MB_OK|MB_ICONSTOP \
        "The ${RUFIN_DISPLAY_NAME} update channel must be direct, scoop, or winget."

invalid_channel_silent:
    SetErrorLevel 3
    Abort

update_channel_done:
    ReadRegStr $LegacyInstallDir HKCU "Software\${RUFIN_PROJECT_NAME}" "InstallDir"
    StrCmp $LegacyInstallDir "" legacy_install_done
    GetFullPathName $LegacyInstallDir "$LegacyInstallDir"
    GetFullPathName $INSTDIR "$INSTDIR"
    StrCmp $LegacyInstallDir $INSTDIR legacy_install_done
    IfFileExists "$LegacyInstallDir\Uninstall.exe" 0 legacy_install_done
    IfFileExists "$LegacyInstallDir\rufin.ico" 0 legacy_install_done
    IfFileExists "$LegacyInstallDir\bin\rufin.exe" legacy_install_owned
    IfFileExists "$LegacyInstallDir\rufin.exe" 0 legacy_install_done

legacy_install_owned:
    StrCpy $LegacyInstallOwned 1

legacy_install_done:
FunctionEnd

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "${RUFIN_STAGE_DIR}/LICENSE"
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
UninstPage custom un.CachePageCreate un.CachePageLeave
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

Section "${RUFIN_DISPLAY_NAME}" RufinSection
    SectionIn RO
    !insertmacro RequireRufinClosed "$INSTDIR\bin\rufin.exe" current_bin runtime_is_running
    !insertmacro RequireRufinClosed "$INSTDIR\rufin.exe" current_root runtime_is_running
    StrCmp $LegacyInstallOwned 1 0 runtime_not_running
    !insertmacro RequireRufinClosed \
        "$LegacyInstallDir\bin\rufin.exe" legacy_bin runtime_is_running
    !insertmacro RequireRufinClosed \
        "$LegacyInstallDir\rufin.exe" legacy_root runtime_is_running
    Goto runtime_not_running

runtime_is_running:
    IfSilent runtime_silent_abort runtime_show_running

runtime_show_running:
    MessageBox MB_OK|MB_ICONEXCLAMATION \
        "Close ${RUFIN_DISPLAY_NAME} and try the installation again."

runtime_silent_abort:
    SetErrorLevel 2
    Abort

runtime_not_running:
    ClearErrors
    RMDir /r "$INSTDIR\bin"
    RMDir /r "$INSTDIR\etc"
    RMDir /r "$INSTDIR\lib"
    RMDir /r "$INSTDIR\libexec"
    RMDir /r "$INSTDIR\share"
    Delete "$INSTDIR\update-channel"
    Delete "$INSTDIR\rufin.exe"
    Delete "$INSTDIR\*.dll"
    Delete "$INSTDIR\gspawn-win64-helper.exe"
    Delete "$INSTDIR\gspawn-win64-helper-console.exe"
    IfErrors runtime_cleanup_failed
    SetOutPath "$INSTDIR"
    File /r "${RUFIN_STAGE_FILES}"
    FileOpen $0 "$INSTDIR\update-channel" w
    IfErrors install_write_failed
    FileWrite $0 "$InstallChannel$\r$\n"
    FileClose $0
    IfErrors install_write_failed
    WriteUninstaller "$INSTDIR\Uninstall.exe"
    IfErrors install_write_failed
    Goto install_written

install_write_failed:
    SetErrorLevel 4
    Abort

runtime_cleanup_failed:
    SetErrorLevel 5
    Abort

install_written:

    StrCmp $LegacyInstallOwned 1 0 legacy_install_removed
    ClearErrors
    RMDir /r "$LegacyInstallDir\bin"
    RMDir /r "$LegacyInstallDir\etc"
    RMDir /r "$LegacyInstallDir\lib"
    RMDir /r "$LegacyInstallDir\libexec"
    RMDir /r "$LegacyInstallDir\share"
    RMDir /r "$LegacyInstallDir\updater"
    Delete "$LegacyInstallDir\rufin.exe"
    Delete "$LegacyInstallDir\*.dll"
    Delete "$LegacyInstallDir\gspawn-win64-helper.exe"
    Delete "$LegacyInstallDir\gspawn-win64-helper-console.exe"
    Delete "$LegacyInstallDir\LICENSE"
    Delete "$LegacyInstallDir\rufin.ico"
    IfErrors runtime_cleanup_failed
    Delete "$LegacyInstallDir\Uninstall.exe"
    IfErrors runtime_cleanup_failed
    RMDir "$LegacyInstallDir"
    ClearErrors

legacy_install_removed:
    DeleteRegKey HKCU "Software\${RUFIN_PROJECT_NAME}"
    ClearErrors
    !insertmacro WriteRufinUninstallRegistration
    IfErrors install_write_failed

    CreateDirectory "$SMPROGRAMS\${RUFIN_DISPLAY_NAME}"
    !insertmacro CreateRufinShortcut \
        "$SMPROGRAMS\${RUFIN_DISPLAY_NAME}\${RUFIN_DISPLAY_NAME}.lnk" \
        "$INSTDIR\bin\rufin.exe" \
        "$INSTDIR\rufin.ico"
    CreateShortcut \
        "$SMPROGRAMS\${RUFIN_DISPLAY_NAME}\Uninstall ${RUFIN_DISPLAY_NAME}.lnk" \
        "$INSTDIR\Uninstall.exe"
    IfFileExists \
        "$DESKTOP\${RUFIN_DISPLAY_NAME}.lnk" \
        create_existing_desktop no_existing_desktop

create_existing_desktop:
    !insertmacro CreateRufinShortcut \
        "$DESKTOP\${RUFIN_DISPLAY_NAME}.lnk" \
        "$INSTDIR\bin\rufin.exe" \
        "$INSTDIR\rufin.ico"

no_existing_desktop:
SectionEnd

Section /o "Desktop shortcut" DesktopSection
    !insertmacro CreateRufinShortcut \
        "$DESKTOP\${RUFIN_DISPLAY_NAME}.lnk" \
        "$INSTDIR\bin\rufin.exe" \
        "$INSTDIR\rufin.ico"
SectionEnd

Function un.onInit
    StrCpy $PurgeCache 0
    ${GetParameters} $0
    ClearErrors
    ${GetOptions} $0 "/PURGE" $1
    IfErrors purge_option_done
    StrCmp $1 "" 0 purge_option_done
    StrCpy $PurgeCache 1

purge_option_done:
FunctionEnd

Function un.CachePageCreate
    nsDialogs::Create 1018
    Pop $0
    ${If} $0 == error
        Abort
    ${EndIf}
    ${NSD_CreateCheckbox} 0 0 100% 14u "Remove ${RUFIN_DISPLAY_NAME}'s cache"
    Pop $PurgeCacheCheckbox
    ${If} $PurgeCache == 1
        ${NSD_Check} $PurgeCacheCheckbox
    ${EndIf}
    nsDialogs::Show
FunctionEnd

Function un.CachePageLeave
    ${NSD_GetState} $PurgeCacheCheckbox $0
    ${If} $0 == ${BST_CHECKED}
        StrCpy $PurgeCache 1
    ${Else}
        StrCpy $PurgeCache 0
    ${EndIf}
FunctionEnd

Section "Uninstall"
    !insertmacro RequireRufinClosed \
        "$INSTDIR\bin\rufin.exe" uninstall_bin uninstall_runtime_is_running
    !insertmacro RequireRufinClosed \
        "$INSTDIR\rufin.exe" uninstall_root uninstall_runtime_is_running
    Goto uninstall_runtime_not_running

uninstall_runtime_is_running:
    IfSilent uninstall_silent_abort uninstall_show_running

uninstall_show_running:
    MessageBox MB_OK|MB_ICONEXCLAMATION \
        "Close ${RUFIN_DISPLAY_NAME} and try the uninstall again."

uninstall_silent_abort:
    SetErrorLevel 2
    Abort

uninstall_runtime_not_running:
    ClearErrors
    Delete "$DESKTOP\${RUFIN_DISPLAY_NAME}.lnk"
    RMDir /r "$SMPROGRAMS\${RUFIN_DISPLAY_NAME}"
    RMDir /r "$INSTDIR\bin"
    RMDir /r "$INSTDIR\etc"
    RMDir /r "$INSTDIR\lib"
    RMDir /r "$INSTDIR\libexec"
    RMDir /r "$INSTDIR\share"
    RMDir /r "$INSTDIR\updater"
    Delete "$INSTDIR\update-channel"
    Delete "$INSTDIR\LICENSE"
    Delete "$INSTDIR\rufin.ico"
    IfErrors uninstall_cleanup_failed

    StrCmp $PurgeCache 1 0 uninstall_cache_preserved
    ClearErrors
    RMDir /r "$LOCALAPPDATA\screwys\${RUFIN_PROJECT_NAME}\cache"
    IfErrors uninstall_cleanup_failed

uninstall_cache_preserved:
    ClearErrors
    ReadRegStr $0 HKCU \
        "Software\Microsoft\Windows\CurrentVersion\Uninstall\${RUFIN_PROJECT_NAME}" \
        "UninstallString"
    IfErrors uninstall_registration_removed
    DeleteRegKey HKCU \
        "Software\Microsoft\Windows\CurrentVersion\Uninstall\${RUFIN_PROJECT_NAME}"
    IfErrors uninstall_cleanup_failed

uninstall_registration_removed:
    ClearErrors
    Delete "$INSTDIR\Uninstall.exe"
    IfErrors uninstall_restore_registration
    RMDir "$INSTDIR"
    DeleteRegKey HKCU "Software\${RUFIN_PROJECT_NAME}"
    ClearErrors
    Goto uninstall_done

uninstall_restore_registration:
    ClearErrors
    !insertmacro WriteRufinUninstallRegistration

uninstall_cleanup_failed:
    SetErrorLevel 5
    Abort

uninstall_done:
SectionEnd
