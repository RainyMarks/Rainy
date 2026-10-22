; Helpers the uninstaller uses, as macros that define a function for a prefix ("un." there), so a test program built from
; tmp/qa/installer can run the same code. Needs LogicLib.nsh and RAINY_EXEC_TIMEOUT_MS.

; ${prefix}rainyExec: runs the command line on the stack and waits for it, at most RAINY_EXEC_TIMEOUT_MS. Leaves its exit
; code, "timeout" (it is left running) or "error" (it did not start) on the stack.
!macro rainyExecFunction prefix
  Function ${prefix}rainyExec
    Exch $R0
    Push $R1
    Push $R2
    Push $R3
    Push $R4
    Push $R5
    StrCpy $R5 "error"
    ; STARTUPINFOW (68 bytes for a 32-bit process, cb first) and PROCESS_INFORMATION (16 bytes).
    System::Alloc 68
    Pop $R1
    System::Call '*$R1(i 68)'
    System::Alloc 16
    Pop $R2
    System::Call 'kernel32::CreateProcessW(p 0, w R0, p 0, p 0, i 0, i 0x08000000, p 0, p 0, p R1, p R2) i .R3'
    ${If} $R3 <> 0
      System::Call '*$R2(p .R3, p .R4)'
      System::Call 'kernel32::WaitForSingleObject(p R3, i ${RAINY_EXEC_TIMEOUT_MS}) i .R5'
      ${If} $R5 = 0
        System::Call 'kernel32::GetExitCodeProcess(p R3, *i .R5)'
      ${Else}
        StrCpy $R5 "timeout"
      ${EndIf}
      System::Call 'kernel32::CloseHandle(p R3)'
      System::Call 'kernel32::CloseHandle(p R4)'
    ${EndIf}
    System::Free $R1
    System::Free $R2
    StrCpy $R0 $R5
    Pop $R5
    Pop $R4
    Pop $R3
    Pop $R2
    Pop $R1
    Exch $R0
  FunctionEnd
!macroend

; ${prefix}rainyRemoveTree: deletes the folder on the stack and everything in it. A link or junction inside is removed
; itself and never followed, and a folder that is a link is left alone.
!macro rainyRemoveTreeFunction prefix
  Function ${prefix}rainyRemoveTree
    Exch $R0
    Push $R1
    Push $R2
    Push $R3
    Push $R4
    System::Call 'kernel32::GetFileAttributesW(w R0) i .R3'
    ${If} $R3 = -1
      Goto done
    ${EndIf}
    IntOp $R4 $R3 & 0x400
    ${If} $R4 <> 0
      Goto done
    ${EndIf}
    ClearErrors
    FindFirst $R1 $R2 "$R0\*.*"
    ${DoUntil} ${Errors}
      ${If} $R2 != "."
      ${AndIf} $R2 != ".."
        System::Call 'kernel32::GetFileAttributesW(w "$R0\$R2") i .R3'
        ${If} $R3 <> -1
          IntOp $R4 $R3 & 0x400
          IntOp $R3 $R3 & 0x10
          ${If} $R4 <> 0
            ${If} $R3 <> 0
              RMDir "$R0\$R2"
            ${Else}
              Delete "$R0\$R2"
            ${EndIf}
          ${ElseIf} $R3 <> 0
            Push "$R0\$R2"
            Call ${prefix}rainyRemoveTree
          ${Else}
            Delete "$R0\$R2"
          ${EndIf}
        ${EndIf}
      ${EndIf}
      ClearErrors
      FindNext $R1 $R2
    ${Loop}
    FindClose $R1
    RMDir "$R0"
    done:
    ClearErrors
    Pop $R4
    Pop $R3
    Pop $R2
    Pop $R1
    Pop $R0
  FunctionEnd
!macroend
