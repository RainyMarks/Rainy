; The application maintenance mode verifies and installs the adjacent offline volumes.
!macro customCheckAppRunning
  InitPluginsDir
  ReadRegStr $R4 HKCU "${UNINSTALL_REGISTRY_KEY}" "InstallLocation"
  ReadRegStr $R5 HKLM "${UNINSTALL_REGISTRY_KEY}" "InstallLocation"
  ${If} $R4 == ""
    StrCpy $R4 "$INSTDIR"
  ${EndIf}
  ${If} $R5 == ""
    StrCpy $R5 "$INSTDIR"
  ${EndIf}
  System::Call 'kernel32::GetCurrentProcessId() i .r9'
  FileOpen $2 "$PLUGINSDIR\rainy-check-running.ps1" w
  FileWrite $2 "param([string]$$InstallRoot, [string]$$PreviousUserRoot, [string]$$PreviousMachineRoot, [int]$$InstallerPid)$\r$\n"
  FileWrite $2 "$$ErrorActionPreference = 'Stop'$\r$\n"
  FileWrite $2 "$$env:PSModulePath = [IO.Path]::Combine($$PSHOME, 'Modules')$\r$\n"
  FileWrite $2 "function Normalize([string]$$value) { return $$value.Replace('/', '\').ToLowerInvariant() }$\r$\n"
  FileWrite $2 "try {$\r$\n"
  FileWrite $2 "  $$roots = @($$InstallRoot, $$PreviousUserRoot, $$PreviousMachineRoot) | ForEach-Object { Normalize ([IO.Path]::GetFullPath($$_).TrimEnd([char]92)) }$\r$\n"
  FileWrite $2 "  foreach ($$row in Get-CimInstance Win32_Process) {$\r$\n"
  FileWrite $2 "    if ($$row.ProcessId -eq $$PID -or $$row.ProcessId -eq $$InstallerPid) { continue }$\r$\n"
  FileWrite $2 "    $$raw = [string]$$row.CommandLine; $$decoded = ''$\r$\n"
  FileWrite $2 "    if ($$raw -match '-(?i:EncodedCommand|enc|ec)\s+([A-Za-z0-9+/=]+)') { try { $$decoded = [Text.Encoding]::Unicode.GetString([Convert]::FromBase64String($$Matches[1])) } catch { $$decoded = '' } }$\r$\n"
  FileWrite $2 "    $$command = Normalize ($$raw + $$decoded.Replace(([string][char]39 + [char]39), [string][char]39)); $$program = Normalize ([string]$$row.ExecutablePath)$\r$\n"
  FileWrite $2 "    foreach ($$root in $$roots) { if ($$program.StartsWith($$root + '\') -or $$command.Contains($$root + '\')) { Write-Output ('Busy: ' + $$row.ProcessId + ' ' + $$row.Name); exit 2 } }$\r$\n"
  FileWrite $2 "  }$\r$\n"
  FileWrite $2 "  exit 0$\r$\n"
  FileWrite $2 "} catch { Write-Output 'Process check failed.'; exit 1 }$\r$\n"
  FileClose $2
  nsExec::ExecToLog '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$PLUGINSDIR\rainy-check-running.ps1" -InstallRoot "$INSTDIR\." -PreviousUserRoot "$R4\." -PreviousMachineRoot "$R5\." -InstallerPid $9'
  Pop $0
  ${If} $0 != 0
    MessageBox MB_OK|MB_ICONEXCLAMATION "请先保存工作并手动关闭 RainyAgent、原生工具及其命令行窗口，再重新运行安装程序。安装程序不会自动结束这些进程。" /SD IDOK
    SetErrorLevel 1
    Abort
  ${EndIf}
!macroend

!macro customInstall
  ${IfNot} ${FileExists} "$EXEDIR\rainy-unit-*.tar.gz"
    DetailPrint "未附带离线工具包，已跳过工具安装并保留现有工具。可在应用的 CTF 工具页按需下载。"
    Goto rainyToolsInstallDone
  ${EndIf}
  DetailPrint "正在校验并安装离线工具包，请稍候..."
  ClearErrors
  ${If} ${Silent}
    ExecWait '"$INSTDIR\RainyAgent.exe" --rainy-install-tools "$EXEDIR\." --rainy-tools-silent' $0
  ${Else}
    ExecWait '"$INSTDIR\RainyAgent.exe" --rainy-install-tools "$EXEDIR\."' $0
  ${EndIf}
  ${If} ${Errors}
    MessageBox MB_OK|MB_ICONSTOP "无法启动离线工具安装。请检查安装目录权限后重新运行安装程序。" /SD IDOK
    SetErrorLevel 1
    Abort
  ${EndIf}
  ${If} $0 == 3
    MessageBox MB_OK|MB_ICONINFORMATION "安装已取消，进度已保留。可重新运行安装程序继续。" /SD IDOK
    SetErrorLevel 3
    Abort
  ${ElseIf} $0 != 0
    MessageBox MB_OK|MB_ICONSTOP "离线工具安装未完成（退出代码 $0）。请关闭正在运行的工具，确认所有 rainy-unit 工具包与安装程序在同一目录，再重新运行安装程序。原工具及恢复记录已保留。" /SD IDOK
    SetErrorLevel $0
    Abort
  ${EndIf}
  DetailPrint "离线工具安装完成。"
  rainyToolsInstallDone:
!macroend

; Explicit application files are removed; tools, runtimes, and recovery records stay in place.
!macro rainyRejectReparse directory
  System::Call 'kernel32::GetFileAttributesW(w "${directory}") i .r0'
  ${If} $0 != -1
    IntOp $1 $0 & 0x400
    ${If} $1 != 0
      MessageBox MB_OK|MB_ICONSTOP "应用文件目录包含重解析点，已停止卸载并保留所有数据：${directory}" /SD IDOK
      SetErrorLevel 1
      Abort
    ${EndIf}
  ${EndIf}
!macroend

!macro rainyDeleteApplicationFile file
  ${If} ${FileExists} "${file}"
    ClearErrors
    Delete "${file}"
    ${If} ${Errors}
      MessageBox MB_OK|MB_ICONSTOP "应用文件仍被占用或无法删除，请关闭 RainyAgent 后重试：${file}" /SD IDOK
      SetErrorLevel 1
      Abort
    ${EndIf}
  ${EndIf}
!macroend

!macro customRemoveFiles
  GetFullPathName $R0 "$INSTDIR"
  GetFullPathName $R1 "$INSTDIR\resources"
  StrCpy $R2 "$R0\resources"
  ${If} $R1 != $R2
    MessageBox MB_OK|MB_ICONSTOP "应用文件目录校验失败，已保留原有文件。" /SD IDOK
    SetErrorLevel 1
    Abort
  ${EndIf}
  !insertmacro rainyRejectReparse "$INSTDIR"
  !insertmacro rainyRejectReparse "$INSTDIR\resources"
  !insertmacro rainyRejectReparse "$INSTDIR\resources\environment"
  !insertmacro rainyRejectReparse "$INSTDIR\locales"
  SetOutPath $TEMP
  !insertmacro rainyDeleteApplicationFile "$INSTDIR\RainyAgent.exe"
  !insertmacro rainyDeleteApplicationFile "$INSTDIR\chrome_100_percent.pak"
  !insertmacro rainyDeleteApplicationFile "$INSTDIR\chrome_200_percent.pak"
  !insertmacro rainyDeleteApplicationFile "$INSTDIR\d3dcompiler_47.dll"
  !insertmacro rainyDeleteApplicationFile "$INSTDIR\dxcompiler.dll"
  !insertmacro rainyDeleteApplicationFile "$INSTDIR\dxil.dll"
  !insertmacro rainyDeleteApplicationFile "$INSTDIR\ffmpeg.dll"
  !insertmacro rainyDeleteApplicationFile "$INSTDIR\icudtl.dat"
  !insertmacro rainyDeleteApplicationFile "$INSTDIR\LICENSE.electron.txt"
  !insertmacro rainyDeleteApplicationFile "$INSTDIR\LICENSES.chromium.html"
  !insertmacro rainyDeleteApplicationFile "$INSTDIR\resources.pak"
  !insertmacro rainyDeleteApplicationFile "$INSTDIR\snapshot_blob.bin"
  !insertmacro rainyDeleteApplicationFile "$INSTDIR\v8_context_snapshot.bin"
  !insertmacro rainyDeleteApplicationFile "$INSTDIR\vk_swiftshader_icd.json"
  !insertmacro rainyDeleteApplicationFile "$INSTDIR\vk_swiftshader.dll"
  !insertmacro rainyDeleteApplicationFile "$INSTDIR\vulkan-1.dll"
  !insertmacro rainyDeleteApplicationFile "$INSTDIR\resources\app.asar"
  !insertmacro rainyDeleteApplicationFile "$INSTDIR\resources\app-update.yml"
  !insertmacro rainyDeleteApplicationFile "$INSTDIR\resources\elevate.exe"
  !insertmacro rainyDeleteApplicationFile "$INSTDIR\resources\icon.ico"
  !insertmacro rainyDeleteApplicationFile "$INSTDIR\resources\install-runtime.py"
  !insertmacro rainyDeleteApplicationFile "$INSTDIR\resources\linux-runtime.json"
  !insertmacro rainyDeleteApplicationFile "$INSTDIR\resources\linux-runtime.tar.gz"
  !insertmacro rainyDeleteApplicationFile "$INSTDIR\resources\native-tools-metadata.json"
  !insertmacro rainyDeleteApplicationFile "$INSTDIR\resources\native-tools-channel.signed.json"
  !insertmacro rainyDeleteApplicationFile "$INSTDIR\resources\native-tools-public-keys.json"
  !insertmacro rainyDeleteApplicationFile "$INSTDIR\resources\optional-modules.json"
  !insertmacro rainyDeleteApplicationFile "$INSTDIR\resources\environment\wsl.3.0.1.0.x64.msi"
  !insertmacro rainyDeleteApplicationFile "$INSTDIR\resources\environment\ubuntu-26.04.1-wsl-amd64.wsl"
  !insertmacro rainyDeleteApplicationFile "$INSTDIR\resources\environment\media-verification.json"
  !insertmacro rainyDeleteApplicationFile "$INSTDIR\locales\*.pak"
  !insertmacro rainyDeleteApplicationFile "$INSTDIR\${UNINSTALL_FILENAME}"
  RMDir "$INSTDIR\resources\environment"
  RMDir "$INSTDIR\resources"
  RMDir "$INSTDIR\locales"
  DetailPrint "本地 Linux 环境、工具和用户数据已保留。"
  ClearErrors
!macroend
