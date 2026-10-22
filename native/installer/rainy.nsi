; Rainy setup and its uninstaller, built by scripts/package-native.mjs with the pinned NSIS 3.04 and nsis-resources 3.4.1.
; Setup keeps the identity of the Rainy 3.x installers (registry keys, folder, RainyAgent.exe, shortcuts, command line
; `--updated /S --force-run`), so it installs over 3.x and over itself without running an earlier uninstaller. /DQA builds
; a test identity ("Rainy QA") with its own keys, a folder below /DQA_ROOT and no shortcuts; it never touches the installed
; Rainy.
Unicode true
ManifestDPIAware true
RequestExecutionLevel user
SetCompressor /SOLID lzma
SetCompressorDictSize 64

!macro rainyRequire name
  !ifndef ${name}
    !error "${name} is not defined; scripts/package-native.mjs passes it."
  !endif
!macroend
!insertmacro rainyRequire VERSION
!insertmacro rainyRequire VI_VERSION
!insertmacro rainyRequire PAYLOAD
!insertmacro rainyRequire PAYLOAD_FILES
!insertmacro rainyRequire OUTFILE
!insertmacro rainyRequire ICON
!insertmacro rainyRequire NSIS_PLUGINS
!insertmacro rainyRequire ESTIMATED_SIZE
!define /ifndef YEAR 2026

!define APP_EXE "RainyAgent.exe"
!define UNINSTALL_EXE "Uninstall RainyAgent.exe"
!define APP_FOLDER "RainyAgent"
!define APP_ID "dev.rainy.agent"
!define SHORTCUT_NAME "Rainy"
!define PUBLISHER "RainyMarks"
!define DESCRIPTION "Rainy: a lightweight AI coding workbench for Windows, WSL and remote Linux over SSH"
!define WEBVIEW2_CLIENT "{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"
!define WEBVIEW2_URL "https://developer.microsoft.com/microsoft-edge/webview2/"
; The 3.x updater starts setup before it quits, so an update waits this long for Rainy's processes to end.
!define UPDATE_WAIT_SECONDS 60
!define MAINTENANCE_TIMEOUT_MS 120000
!ifdef QA
  !insertmacro rainyRequire QA_ROOT
  !define APP_GUID "6a1f0c2e-9d3b-4e57-a8c4-0f2d7b9e5a40"
  !define PRODUCT_NAME "Rainy QA"
!else
  !ifdef QA_ARGS
    !error "QA_ARGS is only for a QA build (/DQA)."
  !endif
  !define APP_GUID "2b4d0a43-506b-52c1-a15c-818e3e1af64d"
  !define PRODUCT_NAME "Rainy"
!endif
!ifdef QA_ARGS
  !define APP_ARGS " ${QA_ARGS}"
!else
  !define APP_ARGS ""
!endif
!define MUTEX "${APP_GUID}"
!define INSTALL_KEY "Software\${APP_GUID}"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_GUID}"

Name "${PRODUCT_NAME}"
OutFile "${OUTFILE}"
BrandingText "${PRODUCT_NAME} ${VERSION}"
ShowInstDetails nevershow
ShowUninstDetails nevershow
AllowSkipFiles off
!addplugindir /x86-unicode "${NSIS_PLUGINS}"

!include LogicLib.nsh
!include FileFunc.nsh
!include x64.nsh
!include WinVer.nsh
!include MUI2.nsh
; The default dialog font draws Chinese in SimSun; YaHei UI reads well in both languages.
SetFont "Microsoft YaHei UI" 9
!include WordFunc.nsh
!include "${PAYLOAD_FILES}"

; Command-line switches, "true" or "false" (StdUtils accepts --name and /name).
Var updated
Var forceRun
Var keepShortcutsFlag
Var noDesktopShortcut
Var deleteAppData
Var forAllUsers
Var forCurrentUser
; UAC: non-zero in the elevated copy setup starts for a per-machine installation, and when running as an administrator.
Var isInner
Var isAdmin
; "user" (HKCU, %LOCALAPPDATA%\Programs) or "all" (HKLM, Program Files).
Var installMode
Var userRoot
Var machineRoot
Var dirArgument
Var existingInstall
Var keepShortcuts
Var oldShortcutName

!define MUI_ICON "${ICON}"
!define MUI_UNICON "${ICON}"
!define MUI_PAGE_CUSTOMFUNCTION_PRE rainyDirectoryPre
!insertmacro MUI_PAGE_DIRECTORY
!define MUI_PAGE_CUSTOMFUNCTION_PRE rainyInstFilesPre
!insertmacro MUI_PAGE_INSTFILES
; The brand's blue with the icon, drawn at twice the page's size so it stays sharp on scaled displays.
!define MUI_WELCOMEFINISHPAGE_BITMAP "${__FILEDIR__}\finish.bmp"
!define MUI_WELCOMEFINISHPAGE_BITMAP_STRETCH AspectFitHeight
!define MUI_FINISHPAGE_RUN
!define MUI_FINISHPAGE_RUN_TEXT "$(rainyRunApp)"
!define MUI_FINISHPAGE_RUN_FUNCTION rainyStartApp
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"
!insertmacro MUI_LANGUAGE "SimpChinese"
!include "${__FILEDIR__}\messages.nsh"

VIProductVersion "${VI_VERSION}"
VIFileVersion "${VI_VERSION}"
VIAddVersionKey /LANG=${LANG_ENGLISH} "ProductName" "${PRODUCT_NAME}"
VIAddVersionKey /LANG=${LANG_ENGLISH} "CompanyName" "${PUBLISHER}"
VIAddVersionKey /LANG=${LANG_ENGLISH} "LegalCopyright" "Copyright © ${YEAR} ${PUBLISHER}"
VIAddVersionKey /LANG=${LANG_ENGLISH} "FileDescription" "${PRODUCT_NAME} Setup"
VIAddVersionKey /LANG=${LANG_ENGLISH} "FileVersion" "${VERSION}"
VIAddVersionKey /LANG=${LANG_ENGLISH} "ProductVersion" "${VERSION}"

; ---------------------------------------------------------------------------------------------------------------------
; Shared by setup and the uninstaller

!macro rainyFlag var name
  Push `${name}`
  StdUtils::TestParameter
  Pop ${var}
!macroend

!macro rainyReadFlags
  !insertmacro rainyFlag $updated "updated"
  !insertmacro rainyFlag $forceRun "force-run"
  !insertmacro rainyFlag $keepShortcutsFlag "keep-shortcuts"
  !insertmacro rainyFlag $noDesktopShortcut "no-desktop-shortcut"
  !insertmacro rainyFlag $deleteAppData "delete-app-data"
  !insertmacro rainyFlag $forAllUsers "allusers"
  !insertmacro rainyFlag $forCurrentUser "currentuser"
!macroend

!macro rainyUacQuery var code
  UAC::_ ${code}s
  Pop ${var}
!macroend

; A per-machine installation needs administrator rights: the UAC plugin runs this program again elevated and this copy
; exits with its exit code. 0x666666 is what an elevated copy that still has no administrator rights exits with.
!macro rainyElevate
  ${Do}
    UAC::_ 0
    ${If} $0 = 0
    ${AndIf} $1 = 3
      ${Continue}
    ${EndIf}
    ${Break}
  ${Loop}
  ${If} $0 = 0
    ${If} $1 = 1
      ${If} $2 = 0x666666
        MessageBox MB_OK|MB_ICONSTOP "$(rainyElevationFailed)" /SD IDOK
        SetErrorLevel 1
      ${Else}
        SetErrorLevel $2
      ${EndIf}
      Quit
    ${EndIf}
  ${Else}
    MessageBox MB_OK|MB_ICONSTOP "$(rainyElevationFailed)" /SD IDOK
    SetErrorLevel 1
    Quit
  ${EndIf}
!macroend

!macro rainyLog text
  Push `${text}`
  !ifdef __UNINSTALL__
    Call un.rainyLogLine
  !else
    Call rainyLogLine
  !endif
!macroend

; Appends a line to setup.log (UTF-16): in the user-data folder, or below QA_ROOT for a QA build. Past 1 MiB it starts over.
!macro rainyLogFunction prefix
  Function ${prefix}rainyLogLine
    Exch $R0
    Push $R1
    Push $R2
    Push $R3
    Push $R4
    Push $R5
    Push $R6
    Push $R7
    Push $R8
    Push $R9
    ${GetTime} "" "L" $R1 $R2 $R3 $R4 $R5 $R6 $R7
    StrCpy $R5 "0$R5" "" -2
    !ifdef QA
      StrCpy $R8 "${QA_ROOT}\setup.log"
    !else
      SetShellVarContext current
      StrCpy $R8 "$APPDATA\${APP_FOLDER}\setup.log"
      ${If} $installMode == "all"
        SetShellVarContext all
      ${EndIf}
    !endif
    ${GetParent} $R8 $R4
    CreateDirectory $R4
    ClearErrors
    FileOpen $R9 $R8 a
    ${IfNot} ${Errors}
      FileSeek $R9 0 END $R4
      ${If} $R4 > 1048576
        FileClose $R9
        FileOpen $R9 $R8 w
      ${EndIf}
      FileWriteUTF16LE /BOM $R9 "$R3-$R2-$R1 $R5:$R6:$R7 $R0$\r$\n"
      FileClose $R9
    ${EndIf}
    ClearErrors
    Pop $R9
    Pop $R8
    Pop $R7
    Pop $R6
    Pop $R5
    Pop $R4
    Pop $R3
    Pop $R2
    Pop $R1
    Pop $R0
  FunctionEnd
!macroend
!insertmacro rainyLogFunction ""
!insertmacro rainyLogFunction "un."

!macro rainyRejectReparse path message
  System::Call 'kernel32::GetFileAttributesW(w "${path}") i .r0'
  ${If} $0 != -1
    IntOp $1 $0 & 0x400
    ${If} $1 != 0
      !insertmacro rainyLog "Stopped: ${path} is a link or another reparse point"
      MessageBox MB_OK|MB_ICONSTOP "${message}${path}" /SD IDOK
      SetErrorLevel 1
      Abort
    ${EndIf}
  ${EndIf}
!macroend

; 3.x checked this before removing anything: `resources` is where its name says, and no folder Rainy deletes from is a
; link to somewhere else.
!macro rainyCheckFolder message
  ${If} ${FileExists} "$INSTDIR\resources\*.*"
    GetFullPathName $R0 "$INSTDIR"
    GetFullPathName $R1 "$INSTDIR\resources"
    ${If} $R1 != "$R0\resources"
      !insertmacro rainyLog "Stopped: $INSTDIR\resources is $R1"
      MessageBox MB_OK|MB_ICONSTOP "$(rainyFolderInvalid)" /SD IDOK
      SetErrorLevel 1
      Abort
    ${EndIf}
  ${EndIf}
  !insertmacro rainyRejectReparse "$INSTDIR" "${message}"
  !insertmacro rainyRejectReparse "$INSTDIR\resources" "${message}"
  !insertmacro rainyRejectReparse "$INSTDIR\resources\environment" "${message}"
  !insertmacro rainyRejectReparse "$INSTDIR\resources\windows-host" "${message}"
  !insertmacro rainyRejectReparse "$INSTDIR\locales" "${message}"
!macroend

; Deletes one file (or a wildcard) Rainy owns; a file that stays stops before anything else changes.
!macro rainyDeleteFile path
  ${If} ${FileExists} "${path}"
    ClearErrors
    Delete "${path}"
    ${If} ${Errors}
      !insertmacro rainyLog "Stopped: ${path} could not be deleted"
      MessageBox MB_OK|MB_ICONSTOP "$(rainyFileBusy)${path}" /SD IDOK
      SetErrorLevel 1
      Abort
    ${EndIf}
  ${EndIf}
!macroend

; Waits until no process runs from the installation (check-running.ps1, never ending one): up to a minute when the
; updater started setup, a few seconds when silent; an interactive run asks to close Rainy and retry.
!macro rainyWaitFunction prefix interactiveSeconds
  Function ${prefix}rainyWaitForExit
    ReadRegStr $R4 HKCU "${INSTALL_KEY}" "InstallLocation"
    !ifdef QA
      StrCpy $R5 ""
    !else
      ReadRegStr $R5 HKLM "${INSTALL_KEY}" "InstallLocation"
    !endif
    ${If} $R4 == ""
      StrCpy $R4 "$INSTDIR"
    ${EndIf}
    ${If} $R5 == ""
      StrCpy $R5 "$INSTDIR"
    ${EndIf}
    ${If} $updated == "true"
      StrCpy $R6 ${UPDATE_WAIT_SECONDS}
    ${ElseIf} ${Silent}
      StrCpy $R6 5
    ${Else}
      StrCpy $R6 ${interactiveSeconds}
    ${EndIf}
    System::Call 'kernel32::GetCurrentProcessId() i .r9'
    InitPluginsDir
    File "/oname=$PLUGINSDIR\rainy-check-running.ps1" "${__FILEDIR__}\check-running.ps1"
    check:
      nsExec::ExecToStack '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$PLUGINSDIR\rainy-check-running.ps1" -InstallRoot "$INSTDIR\." -PreviousUserRoot "$R4\." -PreviousMachineRoot "$R5\." -InstallerPid $9 -WaitSeconds $R6'
      Pop $0
      Pop $1
      StrCmp $0 "0" done
      !insertmacro rainyLog "Rainy is still running ($0): $1"
      IfSilent refuse
      StrCmp $0 "2" 0 refuse
      MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION "$(rainyAppRunningRetry)" IDRETRY check
      Goto stop
    refuse:
      MessageBox MB_OK|MB_ICONEXCLAMATION "$(rainyAppRunning)" /SD IDOK
    stop:
      SetErrorLevel 1
      Abort
    done:
  FunctionEnd
!macroend
!insertmacro rainyWaitFunction "" 0
!insertmacro rainyWaitFunction "un." 5

; ---------------------------------------------------------------------------------------------------------------------
; Setup

; The folder of the program a quoted UninstallString names, or "".
Function rainyQuotedFolder
  Exch $R0
  Push $R1
  Push $R2
  StrCpy $R1 $R0 1
  ${If} $R1 != '"'
    StrCpy $R0 ""
  ${Else}
    StrCpy $R0 $R0 "" 1
    StrCpy $R2 0
    ${Do}
      StrCpy $R1 $R0 1 $R2
      ${If} $R1 == ""
        StrCpy $R0 ""
        ${Break}
      ${ElseIf} $R1 == '"'
        StrCpy $R0 $R0 $R2
        ${Break}
      ${EndIf}
      IntOp $R2 $R2 + 1
    ${Loop}
    ${If} $R0 != ""
      ${GetParent} $R0 $R0
    ${EndIf}
  ${EndIf}
  Pop $R2
  Pop $R1
  Exch $R0
FunctionEnd

; Where Rainy is installed, as 3.x setup finds it: InstallLocation, else the folder of the uninstaller.
!macro rainyFindRoot var root
  ReadRegStr ${var} ${root} "${INSTALL_KEY}" "InstallLocation"
  ${If} ${var} == ""
    ReadRegStr $0 ${root} "${UNINSTALL_KEY}" "UninstallString"
    Push $0
    Call rainyQuotedFolder
    Pop ${var}
  ${EndIf}
!macroend

; Setup speaks Chinese, as Rainy does by default; English only when Rainy was switched to English (desktop.json's
; "locale"), whatever language Windows itself shows.
!macro rainyPickLanguage
  StrCpy $LANGUAGE ${LANG_SIMPCHINESE}
  ClearErrors
  FileOpen $0 "$APPDATA\RainyAgent\desktop.json" r
  ${IfNot} ${Errors}
    ${Do}
      ClearErrors
      FileRead $0 $1
      ${If} ${Errors}
        ${Break}
      ${EndIf}
      ${WordFind} $1 '"locale"' "E+1}" $2
      ${IfNot} ${Errors}
        ${WordFind} $2 '"en"' "E+1{" $3
        ${IfNot} ${Errors}
          StrCpy $LANGUAGE ${LANG_ENGLISH}
        ${EndIf}
        ${Break}
      ${EndIf}
    ${Loop}
    FileClose $0
  ${EndIf}
!macroend

Function .onInit
  !insertmacro rainyPickLanguage
  ; /D= sets $INSTDIR before this runs; everything else is decided here.
  StrCpy $dirArgument $INSTDIR
  ${IfNot} ${RunningX64}
  ${OrIfNot} ${AtLeastWin10}
    MessageBox MB_OK|MB_ICONSTOP "$(rainyWindowsRequired)" /SD IDOK
    SetErrorLevel 1
    Quit
  ${EndIf}
  SetRegView 64
  !insertmacro rainyReadFlags
  !insertmacro rainyUacQuery $isInner 3
  !insertmacro rainyUacQuery $isAdmin 2
  ; One setup at a time, shared with the 3.x installers; the elevated copy runs while its starter holds it.
  ${If} $isInner = 0
    System::Call 'kernel32::CreateMutexW(p 0, i 1, w "${MUTEX}") p .r0 ?e'
    Pop $1
    ${If} $1 = 183
      MessageBox MB_OK|MB_ICONEXCLAMATION "$(rainySetupRunning)" /SD IDOK
      SetErrorLevel 1
      Quit
    ${EndIf}
  ${EndIf}
  !insertmacro rainyFindRoot $userRoot HKCU
  !ifdef QA
    StrCpy $machineRoot ""
  !else
    !insertmacro rainyFindRoot $machineRoot HKLM
  !endif
  ; As in 3.x: /allusers or /currentuser decide; else the only installation there is; else for the current user.
  StrCpy $installMode "user"
  !ifndef QA
    ${If} $forAllUsers == "true"
      StrCpy $installMode "all"
    ${ElseIf} $forCurrentUser != "true"
    ${AndIf} $userRoot == ""
    ${AndIf} $machineRoot != ""
      StrCpy $installMode "all"
    ${EndIf}
  !endif
  ${If} $installMode == "all"
    SetShellVarContext all
    StrCpy $INSTDIR $machineRoot
    ${If} $INSTDIR == ""
      StrCpy $INSTDIR "$PROGRAMFILES64\${APP_FOLDER}"
    ${EndIf}
  ${Else}
    SetShellVarContext current
    StrCpy $INSTDIR $userRoot
    ${If} $INSTDIR == ""
      !ifdef QA
        StrCpy $INSTDIR "${QA_ROOT}\${APP_FOLDER}"
      !else
        ; FOLDERID_UserProgramFiles, normally %LOCALAPPDATA%\Programs.
        StrCpy $0 "$LOCALAPPDATA\Programs"
        StrCpy $2 0
        System::Call 'shell32::SHGetKnownFolderPath(g "{5CD7AEE2-2219-4A67-B85D-6C9CE15660CB}", i 0x8000, p 0, *p .r2) i .r1'
        ${If} $1 = 0
          System::Call 'kernel32::lstrcpynW(w .r0, p r2, i ${NSIS_MAX_STRLEN}) p'
        ${EndIf}
        ${If} $2 != 0
          System::Call 'ole32::CoTaskMemFree(p r2)'
        ${EndIf}
        StrCpy $INSTDIR "$0\${APP_FOLDER}"
      !endif
    ${EndIf}
  ${EndIf}
  ${If} $dirArgument != ""
    StrCpy $INSTDIR $dirArgument
  ${EndIf}
  StrCpy $existingInstall "0"
  ${If} ${FileExists} "$INSTDIR\${APP_EXE}"
    StrCpy $existingInstall "1"
  ${EndIf}
  ${If} $installMode == "all"
  ${AndIf} $isAdmin = 0
    ${If} $isInner <> 0
      SetErrorLevel 0x666666
      Quit
    ${EndIf}
    !insertmacro rainyElevate
  ${EndIf}
FunctionEnd

; An update, or setup over an installation, keeps the folder.
Function rainyDirectoryPre
  ${If} $updated == "true"
  ${OrIf} $existingInstall == "1"
    Abort
  ${EndIf}
FunctionEnd

; A folder picked on the directory page gets its own RainyAgent folder, as in 3.x, unless it holds Rainy already.
Function rainyInstFilesPre
  ${GetFileName} $INSTDIR $0
  ${If} $0 != "${APP_FOLDER}"
  ${AndIfNot} ${FileExists} "$INSTDIR\${APP_EXE}"
    StrCpy $INSTDIR "$INSTDIR\${APP_FOLDER}"
  ${EndIf}
FunctionEnd

; Starts Rainy as the signed-in user (also from an elevated setup): through its Start menu shortcut when there is one, as
; 3.x did, with --updated after an update.
Function rainyStartApp
  StrCpy $1 ""
  ${If} $updated == "true"
    StrCpy $1 "--updated"
  ${EndIf}
  StrCpy $1 `$1${APP_ARGS}`
  StrCpy $0 "$INSTDIR\${APP_EXE}"
  !ifndef QA
    ${If} ${FileExists} "$SMPROGRAMS\${SHORTCUT_NAME}.lnk"
      StrCpy $0 "$SMPROGRAMS\${SHORTCUT_NAME}.lnk"
    ${EndIf}
  !endif
  !insertmacro rainyLog "Starting $0 $1"
  Push $0
  Push "open"
  Push $1
  StdUtils::ExecShellAsUser
  Pop $0
FunctionEnd

; Rainy's previews run in Microsoft Edge WebView2, which Windows 11 has and Windows 10 usually has; setup only says so.
Function rainyCheckWebView2
  ReadRegStr $0 HKLM "SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\${WEBVIEW2_CLIENT}" "pv"
  ${If} $0 == ""
  ${OrIf} $0 == "0.0.0.0"
    ReadRegStr $0 HKCU "Software\Microsoft\EdgeUpdate\Clients\${WEBVIEW2_CLIENT}" "pv"
  ${EndIf}
  ${If} $0 != ""
  ${AndIf} $0 != "0.0.0.0"
    !insertmacro rainyLog "WebView2 runtime $0"
    Return
  ${EndIf}
  !insertmacro rainyLog "The WebView2 runtime was not found"
  MessageBox MB_YESNO|MB_ICONINFORMATION "$(rainyWebView2Missing)" /SD IDNO IDNO done
  Push "${WEBVIEW2_URL}"
  Push "open"
  Push ""
  StdUtils::ExecShellAsUser
  Pop $0
  done:
FunctionEnd

; What Rainy 3.x installed besides resources\windows-host, as its uninstaller deletes it (installer.nsh): an update
; replaces 3.x without running that uninstaller, and the Electron files have no place in 4.0. Then what an earlier 4.x
; installed and this version no longer ships (rainyRemoveRetired, from native/installer/shipped-files.txt). Files this
; setup ships again are deleted first and written anew; resources\windows-host is overwritten, and Rainy removes the files
; there its new runtime.json does not list. runtime\, tools\, .rainy-toolpack\ and anything else not named here stay.
!macro rainyRemoveReplaced
  !insertmacro rainyDeleteFile "$INSTDIR\${APP_EXE}"
  !insertmacro rainyDeleteFile "$INSTDIR\chrome_100_percent.pak"
  !insertmacro rainyDeleteFile "$INSTDIR\chrome_200_percent.pak"
  !insertmacro rainyDeleteFile "$INSTDIR\d3dcompiler_47.dll"
  !insertmacro rainyDeleteFile "$INSTDIR\dxcompiler.dll"
  !insertmacro rainyDeleteFile "$INSTDIR\dxil.dll"
  !insertmacro rainyDeleteFile "$INSTDIR\ffmpeg.dll"
  !insertmacro rainyDeleteFile "$INSTDIR\icudtl.dat"
  !insertmacro rainyDeleteFile "$INSTDIR\LICENSE.electron.txt"
  !insertmacro rainyDeleteFile "$INSTDIR\LICENSES.chromium.html"
  !insertmacro rainyDeleteFile "$INSTDIR\resources.pak"
  !insertmacro rainyDeleteFile "$INSTDIR\snapshot_blob.bin"
  !insertmacro rainyDeleteFile "$INSTDIR\v8_context_snapshot.bin"
  !insertmacro rainyDeleteFile "$INSTDIR\vk_swiftshader_icd.json"
  !insertmacro rainyDeleteFile "$INSTDIR\vk_swiftshader.dll"
  !insertmacro rainyDeleteFile "$INSTDIR\vulkan-1.dll"
  !insertmacro rainyDeleteFile "$INSTDIR\release-integrity.json"
  !insertmacro rainyDeleteFile "$INSTDIR\resources\app.asar"
  !insertmacro rainyDeleteFile "$INSTDIR\resources\app-update.yml"
  !insertmacro rainyDeleteFile "$INSTDIR\resources\elevate.exe"
  !insertmacro rainyDeleteFile "$INSTDIR\resources\icon.ico"
  !insertmacro rainyDeleteFile "$INSTDIR\resources\install-runtime.py"
  !insertmacro rainyDeleteFile "$INSTDIR\resources\linux-runtime.json"
  !insertmacro rainyDeleteFile "$INSTDIR\resources\linux-runtime.tar.gz"
  !insertmacro rainyDeleteFile "$INSTDIR\resources\native-tools-metadata.json"
  !insertmacro rainyDeleteFile "$INSTDIR\resources\native-tools-channel.signed.json"
  !insertmacro rainyDeleteFile "$INSTDIR\resources\native-tools-public-keys.json"
  !insertmacro rainyDeleteFile "$INSTDIR\resources\optional-modules.json"
  !insertmacro rainyDeleteFile "$INSTDIR\resources\environment-component-catalog.json"
  !insertmacro rainyDeleteFile "$INSTDIR\resources\install-environment-component.py"
  !insertmacro rainyDeleteFile "$INSTDIR\resources\release-manifest.signed.json"
  !insertmacro rainyDeleteFile "$INSTDIR\resources\release-public-keys.json"
  !insertmacro rainyDeleteFile "$INSTDIR\resources\environment\wsl.3.0.1.0.x64.msi"
  !insertmacro rainyDeleteFile "$INSTDIR\resources\environment\ubuntu-26.04.1-wsl-amd64.wsl"
  !insertmacro rainyDeleteFile "$INSTDIR\resources\environment\media-verification.json"
  !insertmacro rainyDeleteFile "$INSTDIR\locales\*.pak"
  !insertmacro rainyDeleteFile "$INSTDIR\${UNINSTALL_EXE}"
  RMDir "$INSTDIR\resources\environment"
  RMDir "$INSTDIR\locales"
  !insertmacro rainyRemoveRetired
!macroend

!macro rainyWriteRegistry
  WriteRegStr SHCTX "${INSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr SHCTX "${INSTALL_KEY}" "KeepShortcuts" "true"
  WriteRegStr SHCTX "${INSTALL_KEY}" "ShortcutName" "${SHORTCUT_NAME}"
  ${If} $installMode == "all"
    StrCpy $0 "/allusers"
  ${Else}
    StrCpy $0 "/currentuser"
  ${EndIf}
  WriteRegStr SHCTX "${UNINSTALL_KEY}" "DisplayName" "${PRODUCT_NAME} ${VERSION}"
  WriteRegStr SHCTX "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\${UNINSTALL_EXE}" $0'
  WriteRegStr SHCTX "${UNINSTALL_KEY}" "QuietUninstallString" '"$INSTDIR\${UNINSTALL_EXE}" $0 /S'
  WriteRegStr SHCTX "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr SHCTX "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\${APP_EXE},0"
  WriteRegStr SHCTX "${UNINSTALL_KEY}" "Publisher" "${PUBLISHER}"
  WriteRegStr SHCTX "${UNINSTALL_KEY}" "Comments" "${DESCRIPTION}"
  WriteRegDWORD SHCTX "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD SHCTX "${UNINSTALL_KEY}" "NoRepair" 1
  WriteRegDWORD SHCTX "${UNINSTALL_KEY}" "EstimatedSize" ${ESTIMATED_SIZE}
!macroend

; A fresh install (or a reinstall) creates the Start menu and desktop shortcuts with Rainy's AppUserModelID; an update keeps
; what the user has, follows a renamed shortcut and never brings back a deleted one.
!macro rainyShortcuts
  StrCpy $R0 "$SMPROGRAMS\${SHORTCUT_NAME}.lnk"
  StrCpy $R1 "$DESKTOP\${SHORTCUT_NAME}.lnk"
  StrCpy $R2 "$SMPROGRAMS\$oldShortcutName.lnk"
  StrCpy $R3 "$DESKTOP\$oldShortcutName.lnk"
  ${If} $keepShortcuts == "false"
    CreateShortCut "$R0" "$INSTDIR\${APP_EXE}" "" "$INSTDIR\${APP_EXE}" 0 "" "" "${DESCRIPTION}"
    ClearErrors
    WinShell::SetLnkAUMI "$R0" "${APP_ID}"
    ${If} $noDesktopShortcut != "true"
      CreateShortCut "$R1" "$INSTDIR\${APP_EXE}" "" "$INSTDIR\${APP_EXE}" 0 "" "" "${DESCRIPTION}"
      ClearErrors
      WinShell::SetLnkAUMI "$R1" "${APP_ID}"
    ${EndIf}
  ${Else}
    ${If} $R2 != $R0
    ${AndIf} ${FileExists} "$R2"
      Rename "$R2" "$R0"
      WinShell::UninstShortcut "$R2"
      WinShell::SetLnkAUMI "$R0" "${APP_ID}"
    ${EndIf}
    ${If} $noDesktopShortcut != "true"
    ${AndIf} $R3 != $R1
    ${AndIf} ${FileExists} "$R3"
      Rename "$R3" "$R1"
      WinShell::UninstShortcut "$R3"
      WinShell::SetLnkAUMI "$R1" "${APP_ID}"
    ${EndIf}
  ${EndIf}
  ClearErrors
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'
!macroend

Section "-Rainy"
  !insertmacro rainyLog "Setup ${VERSION} for $installMode into $INSTDIR (updated $updated, force-run $forceRun)"
  ${If} ${FileExists} "$INSTDIR\*.*"
    !insertmacro rainyCheckFolder "$(rainyReparseInstall)"
    Call rainyWaitForExit
  ${EndIf}
  ReadRegStr $oldShortcutName SHCTX "${INSTALL_KEY}" "ShortcutName"
  ${If} $oldShortcutName == ""
    StrCpy $oldShortcutName "${SHORTCUT_NAME}"
  ${EndIf}
  StrCpy $keepShortcuts "false"
  ${If} $updated == "true"
    ReadRegStr $0 SHCTX "${INSTALL_KEY}" "KeepShortcuts"
    ${If} $0 == "true"
    ${AndIf} ${FileExists} "$INSTDIR\${APP_EXE}"
      StrCpy $keepShortcuts "true"
    ${EndIf}
  ${EndIf}
  !insertmacro rainyRemoveReplaced
  SetOutPath "$INSTDIR"
  File /r "${PAYLOAD}\*.*"
  WriteUninstaller "$INSTDIR\${UNINSTALL_EXE}"
  !insertmacro rainyWriteRegistry
  !ifndef QA
    !insertmacro rainyShortcuts
  !endif
  ${If} $updated != "true"
    Call rainyCheckWebView2
  ${EndIf}
  !insertmacro rainyLog "Installed ${PRODUCT_NAME} ${VERSION} in $INSTDIR"
  ${If} ${Silent}
  ${AndIf} $forceRun == "true"
    HideWindow
    Call rainyStartApp
  ${EndIf}
SectionEnd

Function .onInstFailed
  !insertmacro rainyLog "Setup did not complete"
FunctionEnd

; ---------------------------------------------------------------------------------------------------------------------
; Uninstaller: `Uninstall RainyAgent.exe`, as 3.x named it. Setup never runs it; 3.x setup does when going back to 3.x,
; with /S /KEEP_APP_DATA /currentuser --keep-shortcuts --updated _?=<folder>.

; Rainy.exe runs each uninstall step for at most this long; one that takes longer is left running.
!define RAINY_EXEC_TIMEOUT_MS ${MAINTENANCE_TIMEOUT_MS}
!include "${__FILEDIR__}\helpers.nsh"
!insertmacro rainyExecFunction "un."
!insertmacro rainyRemoveTreeFunction "un."

; When RainyAgent.exe is missing or could not remove its Explorer entries, what 3.x's uninstaller deleted goes here, each
; only while it still names this installation's RainyAgent.exe: the three menu commands, "Open with" for the program,
; and its Default apps registration (RegisteredApplications and Software\RainyMarks\Rainy).
!macro rainyRemoveShellFallback
  ReadRegStr $1 HKCU "Software\Classes\*\shell\Rainy\command" ""
  ${If} $1 == `"$INSTDIR\${APP_EXE}" "%1"`
    DeleteRegKey HKCU "Software\Classes\*\shell\Rainy"
  ${EndIf}
  ReadRegStr $1 HKCU "Software\Classes\Directory\shell\Rainy\command" ""
  ${If} $1 == `"$INSTDIR\${APP_EXE}" "%V"`
    DeleteRegKey HKCU "Software\Classes\Directory\shell\Rainy"
  ${EndIf}
  ReadRegStr $1 HKCU "Software\Classes\Directory\Background\shell\Rainy\command" ""
  ${If} $1 == `"$INSTDIR\${APP_EXE}" "%V"`
    DeleteRegKey HKCU "Software\Classes\Directory\Background\shell\Rainy"
  ${EndIf}
  ReadRegStr $1 HKCU "Software\Classes\Applications\${APP_EXE}\shell\open\command" ""
  ${If} $1 == `"$INSTDIR\${APP_EXE}" "%1"`
    DeleteRegKey HKCU "Software\Classes\Applications\${APP_EXE}"
  ${EndIf}
  ReadRegStr $1 HKCU "Software\RainyMarks\Rainy\Capabilities" "ApplicationIcon"
  ${If} $1 == `"$INSTDIR\${APP_EXE}",0`
    ReadRegStr $1 HKCU "Software\RegisteredApplications" "Rainy"
    ${If} $1 == "Software\RainyMarks\Rainy\Capabilities"
      DeleteRegValue HKCU "Software\RegisteredApplications" "Rainy"
    ${EndIf}
    DeleteRegKey HKCU "Software\RainyMarks\Rainy"
    DeleteRegKey /ifempty HKCU "Software\RainyMarks"
  ${EndIf}
!macroend

Function un.onInit
  !insertmacro rainyPickLanguage
  SetRegView 64
  !insertmacro rainyReadFlags
  !insertmacro rainyUacQuery $isInner 3
  !insertmacro rainyUacQuery $isAdmin 2
  StrCpy $installMode "user"
  !ifndef QA
    ${If} $forAllUsers == "true"
      StrCpy $installMode "all"
    ${ElseIf} $forCurrentUser != "true"
      ReadRegStr $0 HKLM "${INSTALL_KEY}" "InstallLocation"
      ${If} $0 == $INSTDIR
        StrCpy $installMode "all"
      ${EndIf}
    ${EndIf}
  !endif
  ${If} $installMode == "all"
    SetShellVarContext all
    ${If} $isAdmin = 0
      ${If} $isInner <> 0
        SetErrorLevel 0x666666
        Quit
      ${EndIf}
      !insertmacro rainyElevate
    ${EndIf}
  ${Else}
    SetShellVarContext current
  ${EndIf}
FunctionEnd

Section "un.Rainy"
  !insertmacro rainyLog "Uninstall ${VERSION} for $installMode from $INSTDIR (updated $updated)"
  !insertmacro rainyCheckFolder "$(rainyReparseUninstall)"
  Call un.rainyWaitForExit
  SetOutPath "$TEMP"
  ; Rainy removes what it wrote itself: its Explorer entries (not on an update) and resources\windows-host, file by
  ; file from its runtime.json. Both need RainyAgent.exe and WebView2Loader.dll, so they run before any file goes.
  ${If} $updated != "true"
    StrCpy $0 "missing"
    ${If} ${FileExists} "$INSTDIR\${APP_EXE}"
      Push `"$INSTDIR\${APP_EXE}" --rainy-unregister-shell${APP_ARGS}`
      Call un.rainyExec
      Pop $0
    ${EndIf}
    !insertmacro rainyLog "--rainy-unregister-shell: $0"
    ${If} $0 != "0"
      !insertmacro rainyRemoveShellFallback
    ${EndIf}
  ${EndIf}
  ${If} ${FileExists} "$INSTDIR\${APP_EXE}"
  ${AndIf} ${FileExists} "$INSTDIR\resources\windows-host\runtime.json"
    Push `"$INSTDIR\${APP_EXE}" --rainy-remove-runtime${APP_ARGS}`
    Call un.rainyExec
    Pop $0
    !insertmacro rainyLog "--rainy-remove-runtime: $0"
  ${EndIf}
  !insertmacro rainyRemoveOwnFiles
  Delete "$INSTDIR\${UNINSTALL_EXE}"
  !insertmacro rainyRemoveOwnFolders
  RMDir "$INSTDIR"
  !ifndef QA
    ${If} $keepShortcutsFlag != "true"
      ReadRegStr $0 SHCTX "${INSTALL_KEY}" "ShortcutName"
      ${If} $0 == ""
        StrCpy $0 "${SHORTCUT_NAME}"
      ${EndIf}
      WinShell::UninstAppUserModelId "${APP_ID}"
      WinShell::UninstShortcut "$DESKTOP\$0.lnk"
      Delete "$DESKTOP\$0.lnk"
      WinShell::UninstShortcut "$SMPROGRAMS\$0.lnk"
      Delete "$SMPROGRAMS\$0.lnk"
    ${EndIf}
  !endif
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'
  DeleteRegKey SHCTX "${UNINSTALL_KEY}"
  DeleteRegKey SHCTX "${INSTALL_KEY}"
  !insertmacro rainyLog "Uninstalled ${PRODUCT_NAME} from $INSTDIR"
  ${If} $deleteAppData == "true"
  ${AndIf} $updated != "true"
    !ifdef QA
      !insertmacro rainyLog "A QA build keeps the user data"
    !else
      SetShellVarContext current
      Push "$APPDATA\${APP_FOLDER}"
      Call un.rainyRemoveTree
      ${If} $installMode == "all"
        SetShellVarContext all
      ${EndIf}
    !endif
  ${Else}
    DetailPrint "$(rainyDataKept)"
  ${EndIf}
SectionEnd
