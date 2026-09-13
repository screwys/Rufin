!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "TextFunc.nsh"
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
OutFile "${RUFIN_OUTPUT_DIR}\${RUFIN_PROJECT_NAME}-${RUFIN_VERSION}-setup.exe"
InstallDir "$LOCALAPPDATA\Programs\${RUFIN_PROJECT_NAME}"
InstallDirRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${RUFIN_PROJECT_NAME}" "InstallLocation"
RequestExecutionLevel user
SetCompressor /SOLID lzma
Icon "${RUFIN_ASSET_DIR}\rufin.ico"
UninstallIcon "${RUFIN_ASSET_DIR}\rufin.ico"

VIProductVersion "${RUFIN_VERSION_QUAD}"
VIAddVersionKey /LANG=1033 "ProductName" "${RUFIN_DISPLAY_NAME}"
VIAddVersionKey /LANG=1033 "CompanyName" "screwy"
VIAddVersionKey /LANG=1033 "FileDescription" "${RUFIN_DISPLAY_NAME} installer"
VIAddVersionKey /LANG=1033 "FileVersion" "${RUFIN_VERSION}"
VIAddVersionKey /LANG=1033 "ProductVersion" "${RUFIN_VERSION}"
VIAddVersionKey /LANG=1033 "LegalCopyright" "GPL-3.0-or-later"

!define MUI_ABORTWARNING
!define MUI_ICON "${RUFIN_ASSET_DIR}\rufin.ico"
!define MUI_UNICON "${RUFIN_ASSET_DIR}\rufin.ico"
!define MUI_WELCOMEFINISHPAGE_BITMAP "${RUFIN_ASSET_DIR}\wizard.bmp"
!define MUI_WELCOMEPAGE_TITLE "$(WelcomeTitle)"
!define MUI_WELCOMEPAGE_TEXT "$(WelcomeText)"
!define MUI_FINISHPAGE_TITLE "$(FinishTitle)"
!define MUI_FINISHPAGE_TEXT "$(FinishText)"
!define MUI_FINISHPAGE_RUN "$INSTDIR\bin\rufin.exe"
!define MUI_FINISHPAGE_SHOWREADME
!define MUI_FINISHPAGE_SHOWREADME_TEXT "$(DesktopShortcut)"
!define MUI_FINISHPAGE_SHOWREADME_FUNCTION CreateDesktopShortcut
!define MUI_FINISHPAGE_SHOWREADME_NOTCHECKED
!define MUI_LANGDLL_REGISTRY_ROOT HKCU
!define MUI_LANGDLL_REGISTRY_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${RUFIN_PROJECT_NAME}"
!define MUI_LANGDLL_REGISTRY_VALUENAME "InstallerLanguage"

Var LegacyInstallDir
Var LegacyInstallOwned
Var UpdateMode
Var UpdateWaitAttempts
Var PurgeCache
Var PurgeCacheCheckbox
Var CleanupDir

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "${RUFIN_STAGE_DIR}\LICENSE"
!define MUI_PAGE_CUSTOMFUNCTION_LEAVE ValidateDestination
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
UninstPage custom un.CachePageCreate un.CachePageLeave
!insertmacro MUI_UNPAGE_INSTFILES

!include "${RUFIN_LANGUAGES_FILE}"
!insertmacro MUI_RESERVEFILE_LANGDLL

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
    ; Windows permits renaming a running executable. Request exclusive write
    ; access instead, without changing the installed executable's name.
    System::Call 'kernel32::CreateFileW(w "${EXECUTABLE}", i 0x40000000, i 0, p 0, i 3, i 0, p 0) p .r0'
    StrCmp $0 -1 ${RUNNING_LABEL}
    System::Call 'kernel32::CloseHandle(p r0)'

${LABEL}_not_running:
!macroend

!macro RemoveInstalledFilesFunction PREFIX
Function ${PREFIX}RemoveInstalledFiles
    ; 0.15.0/0.15.1 shipped these Windows runtime files by mistake.
    ; Remove them independently of the inventory: an extracted upgrade can
    ; replace that inventory without removing files absent from the new bundle.
    ClearErrors
    Delete "$CleanupDir\bin\advapi32.dll"
    Delete "$CleanupDir\bin\bcrypt.dll"
    Delete "$CleanupDir\bin\bcryptprimitives.dll"
    Delete "$CleanupDir\bin\cfgmgr32.dll"
    Delete "$CleanupDir\bin\combase.dll"
    Delete "$CleanupDir\bin\comctl32.dll"
    Delete "$CleanupDir\bin\comdlg32.dll"
    Delete "$CleanupDir\bin\crypt32.dll"
    Delete "$CleanupDir\bin\d3d11.dll"
    Delete "$CleanupDir\bin\d3d12.dll"
    Delete "$CleanupDir\bin\dcomp.dll"
    Delete "$CleanupDir\bin\dnsapi.dll"
    Delete "$CleanupDir\bin\dsound.dll"
    Delete "$CleanupDir\bin\dwmapi.dll"
    Delete "$CleanupDir\bin\dwrite.dll"
    Delete "$CleanupDir\bin\dxgi.dll"
    Delete "$CleanupDir\bin\gdi32.dll"
    Delete "$CleanupDir\bin\gdiplus.dll"
    Delete "$CleanupDir\bin\glu32.dll"
    Delete "$CleanupDir\bin\hid.dll"
    Delete "$CleanupDir\bin\imm32.dll"
    Delete "$CleanupDir\bin\iphlpapi.dll"
    Delete "$CleanupDir\bin\kernel32.dll"
    Delete "$CleanupDir\bin\kernelbase.dll"
    Delete "$CleanupDir\bin\ktmw32.dll"
    Delete "$CleanupDir\bin\microsoft.internal.warppal.dll"
    Delete "$CleanupDir\bin\msdmo.dll"
    Delete "$CleanupDir\bin\msimg32.dll"
    Delete "$CleanupDir\bin\msvcp_win.dll"
    Delete "$CleanupDir\bin\msvcrt.dll"
    Delete "$CleanupDir\bin\ncrypt.dll"
    Delete "$CleanupDir\bin\ntdll.dll"
    Delete "$CleanupDir\bin\ole32.dll"
    Delete "$CleanupDir\bin\oleaut32.dll"
    Delete "$CleanupDir\bin\opengl32.dll"
    Delete "$CleanupDir\bin\propsys.dll"
    Delete "$CleanupDir\bin\resampledmo.dll"
    Delete "$CleanupDir\bin\rpcrt4.dll"
    Delete "$CleanupDir\bin\sechost.dll"
    Delete "$CleanupDir\bin\secur32.dll"
    Delete "$CleanupDir\bin\setupapi.dll"
    Delete "$CleanupDir\bin\shcore.dll"
    Delete "$CleanupDir\bin\shell32.dll"
    Delete "$CleanupDir\bin\shlwapi.dll"
    Delete "$CleanupDir\bin\user32.dll"
    Delete "$CleanupDir\bin\userenv.dll"
    Delete "$CleanupDir\bin\usp10.dll"
    Delete "$CleanupDir\bin\version.dll"
    Delete "$CleanupDir\bin\win32u.dll"
    Delete "$CleanupDir\bin\winmm.dll"
    Delete "$CleanupDir\bin\wldap32.dll"
    Delete "$CleanupDir\bin\ws2_32.dll"
    Delete "$CleanupDir\bin\wsock32.dll"
    IfErrors inventory_failed

    StrCpy $0 "$CleanupDir\install-files.txt"
    IfFileExists "$0" inventory_ready
    ; Older releases did not record their files. Apart from the retired files
    ; above, only remove filenames present in this package.
    InitPluginsDir
    File /oname=$PLUGINSDIR\install-files.txt "${RUFIN_FILES_FILE}"
    StrCpy $0 "$PLUGINSDIR\install-files.txt"

inventory_ready:
    ClearErrors
    FileOpen $1 "$0" r
    IfErrors inventory_failed
inventory_next:
    ClearErrors
    FileReadUTF16LE $1 $2
    IfErrors inventory_done
    ${TrimNewLines} $2 $2
    StrCpy $3 $2 1
    StrCpy $2 $2 "" 1
    StrCmp $3 "F" inventory_file
    ; RMDir without /r leaves directories containing unrelated files intact.
    RMDir "$CleanupDir\$2"
    Goto inventory_next
inventory_file:
    Delete "$CleanupDir\$2"
    IfErrors inventory_close_failed
    Goto inventory_next
inventory_done:
    FileClose $1
    ClearErrors
    Delete "$CleanupDir\install-files.txt"
    Return
inventory_close_failed:
    FileClose $1
inventory_failed:
    SetErrors
FunctionEnd
!macroend

!insertmacro RemoveInstalledFilesFunction ""
!insertmacro RemoveInstalledFilesFunction "un."

!macro WriteRufinUninstallRegistration
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${RUFIN_PROJECT_NAME}" \
        "DisplayName" "${RUFIN_DISPLAY_NAME}"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${RUFIN_PROJECT_NAME}" \
        "DisplayVersion" "${RUFIN_VERSION}"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${RUFIN_PROJECT_NAME}" \
        "InstallLocation" "$INSTDIR"
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
    StrCpy $LegacyInstallOwned 0
    StrCpy $UpdateMode 0
    StrCpy $UpdateWaitAttempts 0
    ${GetParameters} $0
    ClearErrors
    ${GetOptions} $0 "/RUFINUPDATE=" $1
    IfErrors update_mode_done
    StrCmp $1 "1" update_mode_value_valid invalid_update_mode

update_mode_value_valid:
    IfSilent update_mode_enabled invalid_update_mode

invalid_update_mode:
    IfSilent invalid_update_mode_silent invalid_update_mode_message

invalid_update_mode_message:
    MessageBox MB_OK|MB_ICONSTOP \
        "$(InvalidUpdate)"

invalid_update_mode_silent:
    SetErrorLevel 3
    Abort

update_mode_enabled:
    StrCpy $UpdateMode 1

update_mode_done:
    !insertmacro MUI_LANGDLL_DISPLAY
FunctionEnd

Function DetectPreviousInstall
    StrCpy $LegacyInstallOwned 0
    ClearErrors
    ReadRegStr $LegacyInstallDir HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${RUFIN_PROJECT_NAME}" "InstallLocation"
    StrCmp $LegacyInstallDir "" 0 previous_install_found
    ReadRegStr $LegacyInstallDir HKCU "Software\${RUFIN_PROJECT_NAME}" "InstallDir"
    StrCmp $LegacyInstallDir "" 0 previous_install_found
    StrCpy $LegacyInstallDir "$LOCALAPPDATA\Programs\${RUFIN_PROJECT_NAME}"
previous_install_found:
    StrCmp $LegacyInstallDir "" legacy_install_done
    IfFileExists "$LegacyInstallDir\Uninstall.exe" 0 legacy_install_done
    IfFileExists "$LegacyInstallDir\rufin.ico" 0 legacy_install_done
    IfFileExists "$LegacyInstallDir\bin\rufin.exe" legacy_install_owned
    IfFileExists "$LegacyInstallDir\rufin.exe" 0 legacy_install_done

legacy_install_owned:
    StrCpy $LegacyInstallOwned 1

legacy_install_done:
FunctionEnd

Function ValidateDestination
    Call DetectPreviousInstall
    ; Moving into or above the old installation would make its cleanup remove
    ; the new payload. Both folders must be separate when changing location.
    StrCmp $LegacyInstallOwned 1 0 destination_valid
    ; Compare normalized directories without changing NSIS's chosen destination.
    System::Call 'kernel32::GetFullPathNameW(w "$INSTDIR\.\", i ${NSIS_MAX_STRLEN}, w .r0, p 0)'
    System::Call 'kernel32::GetFullPathNameW(w "$LegacyInstallDir\.\", i ${NSIS_MAX_STRLEN}, w .r1, p 0)'
    ${If} $0 == $1
        StrCpy $LegacyInstallOwned 0
        Goto destination_valid
    ${EndIf}
    StrLen $2 $0
    StrCpy $3 $1 $2
    StrCmp $3 $0 invalid_destination
    StrLen $2 $1
    StrCpy $3 $0 $2
    StrCmp $3 $1 invalid_destination

destination_valid:
    ClearErrors
    Return
invalid_destination:
    IfSilent +2
    MessageBox MB_OK|MB_ICONEXCLAMATION \
        "$(DirectoryConflict)"
    SetErrorLevel 7
    Abort
FunctionEnd

Function .onInstSuccess
    StrCmp $UpdateMode 1 0 update_launch_done
    SetOutPath "$INSTDIR"
    ClearErrors
    Exec '"$INSTDIR\bin\rufin.exe"'
    IfErrors 0 update_launch_done
    SetErrorLevel 6

update_launch_done:
FunctionEnd

Function .onInstFailed
    StrCmp $UpdateMode 1 0 update_failure_done
    !insertmacro RequireRufinClosed \
        "$INSTDIR\bin\rufin.exe" update_failure_bin update_failure_done
    SetOutPath "$INSTDIR"
    Exec '"$INSTDIR\bin\rufin.exe"'

update_failure_done:
FunctionEnd

Section "${RUFIN_DISPLAY_NAME}" RufinSection
    SectionIn RO
    Call ValidateDestination
    SetErrorLevel 0

runtime_check:
    !insertmacro RequireRufinClosed "$INSTDIR\bin\rufin.exe" current_bin runtime_is_running
    !insertmacro RequireRufinClosed "$INSTDIR\rufin.exe" current_root runtime_is_running
    StrCmp $LegacyInstallOwned 1 0 runtime_not_running
    !insertmacro RequireRufinClosed \
        "$LegacyInstallDir\bin\rufin.exe" legacy_bin runtime_is_running
    !insertmacro RequireRufinClosed \
        "$LegacyInstallDir\rufin.exe" legacy_root runtime_is_running
    Goto runtime_not_running

runtime_is_running:
    StrCmp $UpdateMode 1 runtime_wait
    IfSilent runtime_silent_abort runtime_show_running

runtime_wait:
    IntOp $UpdateWaitAttempts $UpdateWaitAttempts + 1
    IntCmp $UpdateWaitAttempts 150 runtime_wait_exhausted runtime_wait_more runtime_wait_exhausted

runtime_wait_more:
    Sleep 100
    Goto runtime_check

runtime_wait_exhausted:
    IfSilent runtime_silent_abort runtime_show_running

runtime_show_running:
    MessageBox MB_OK|MB_ICONEXCLAMATION \
        "$(CloseToInstall)"

runtime_silent_abort:
    SetErrorLevel 2
    Abort

runtime_not_running:
    SetOutPath "$TEMP"
    StrCpy $CleanupDir "$INSTDIR"
    Call RemoveInstalledFiles
    IfErrors runtime_cleanup_failed
    SetOutPath "$INSTDIR"
    File /r "${RUFIN_STAGE_FILES}"
    File /oname=install-files.txt "${RUFIN_FILES_FILE}"
    WriteUninstaller "$INSTDIR\Uninstall.exe"
    IfErrors install_write_failed
    Goto install_written

install_write_failed:
    SetErrorLevel 4
    Abort "$(InstallWriteFailed)"

runtime_cleanup_failed:
    SetErrorLevel 5
    Abort "$(CleanupFailed)"

install_written:

    StrCmp $LegacyInstallOwned 1 0 legacy_install_removed
    StrCpy $CleanupDir "$LegacyInstallDir"
    Call RemoveInstalledFiles
    IfErrors runtime_cleanup_failed
    Delete "$LegacyInstallDir\rufin.exe"
    Delete "$LegacyInstallDir\gspawn-win64-helper.exe"
    Delete "$LegacyInstallDir\gspawn-win64-helper-console.exe"
    Delete "$LegacyInstallDir\update-channel"
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

Function CreateDesktopShortcut
    !insertmacro CreateRufinShortcut \
        "$DESKTOP\${RUFIN_DISPLAY_NAME}.lnk" \
        "$INSTDIR\bin\rufin.exe" \
        "$INSTDIR\rufin.ico"
FunctionEnd

Function un.onInit
    !insertmacro MUI_UNGETLANGUAGE
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
    ${NSD_CreateCheckbox} 0 0 100% 14u "$(RemoveCache)"
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
        "$(CloseToUninstall)"

uninstall_silent_abort:
    SetErrorLevel 2
    Abort

uninstall_runtime_not_running:
    SetOutPath "$TEMP"
    StrCpy $CleanupDir "$INSTDIR"
    Call un.RemoveInstalledFiles
    IfErrors uninstall_cleanup_failed
    ClearErrors
    Delete "$DESKTOP\${RUFIN_DISPLAY_NAME}.lnk"
    Delete "$SMPROGRAMS\${RUFIN_DISPLAY_NAME}\${RUFIN_DISPLAY_NAME}.lnk"
    Delete "$SMPROGRAMS\${RUFIN_DISPLAY_NAME}\Uninstall ${RUFIN_DISPLAY_NAME}.lnk"
    IfErrors uninstall_cleanup_failed
    RMDir "$SMPROGRAMS\${RUFIN_DISPLAY_NAME}"
    ClearErrors

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
    Abort "$(CleanupFailed)"

uninstall_done:
SectionEnd
