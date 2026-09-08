Var PapStartupLockHandle
Var PapStartupLockOverlapped
Var PapStartupLockHeld
Var PapStartupLockPath

!macro PAP_RELEASE_STARTUP_LOCK
  ${If} $PapStartupLockHeld == 1
    System::Call 'kernel32::UnlockFileEx(p $PapStartupLockHandle, i 0, i 1, i 0, p $PapStartupLockOverlapped)'
  ${EndIf}
  ${If} $PapStartupLockHandle != ""
  ${AndIf} $PapStartupLockHandle != -1
    System::Call 'kernel32::CloseHandle(p $PapStartupLockHandle)'
  ${EndIf}
  ${If} $PapStartupLockOverlapped != ""
    System::Free $PapStartupLockOverlapped
  ${EndIf}
  StrCpy $PapStartupLockHandle ""
  StrCpy $PapStartupLockOverlapped ""
  StrCpy $PapStartupLockHeld 0
!macroend

!macro PAP_FAIL MESSAGE
  !insertmacro PAP_RELEASE_STARTUP_LOCK
  DetailPrint "${MESSAGE}"
  ${IfNot} ${Silent}
    MessageBox MB_ICONSTOP|MB_OK "${MESSAGE}"
  ${EndIf}
  SetErrorLevel 1
  Abort
!macroend

!macro PAP_ACQUIRE_STARTUP_LOCK
  ${If} $PapStartupLockHeld != 1
    StrCpy $PapStartupLockHandle ""
    StrCpy $PapStartupLockOverlapped ""
    StrCpy $PapStartupLockHeld 0

    ReadEnvStr $PapStartupLockPath "PRIVATE_AI_PROXY_HOME"
    ${If} $PapStartupLockPath == ""
      ReadEnvStr $PapStartupLockPath "APPDATA"
      ${If} $PapStartupLockPath == ""
        !insertmacro PAP_FAIL "APPDATA is not set, so the backend startup lock cannot be located."
      ${EndIf}
      StrCpy $PapStartupLockPath "$PapStartupLockPath\${BUNDLEID}"
    ${Else}
      StrCpy $PapStartupLockPath "$PapStartupLockPath\.private-ai-proxy"
    ${EndIf}
    ClearErrors
    CreateDirectory "$PapStartupLockPath"
    ${If} ${Errors}
      !insertmacro PAP_FAIL "Cannot create the Private AI Proxy data directory at $PapStartupLockPath."
    ${EndIf}
    StrCpy $PapStartupLockPath "$PapStartupLockPath\startup.lock"

    System::Call 'kernel32::CreateFileW(w "$PapStartupLockPath", i 0xC0000000, i 7, p 0, i 4, i 0x80, p 0) p.R0 ?e'
    Pop $R2
    StrCpy $PapStartupLockHandle $R0
    ${If} $PapStartupLockHandle == -1
      StrCpy $PapStartupLockHandle ""
      !insertmacro PAP_FAIL "Cannot open the backend startup lock at $PapStartupLockPath (Windows error $R2)."
    ${EndIf}

    System::Call '*(p 0, p 0, i 0, i 0, p 0) p.R0'
    StrCpy $PapStartupLockOverlapped $R0
    ${If} $PapStartupLockOverlapped == 0
      StrCpy $PapStartupLockOverlapped ""
      !insertmacro PAP_FAIL "Cannot allocate the backend startup lock state."
    ${EndIf}

    StrCpy $R3 200
    ${Do}
      ; Byte zero overlaps Rust File::try_lock's whole-file lock. Never delete the lock file.
      System::Call 'kernel32::LockFileEx(p $PapStartupLockHandle, i 3, i 0, i 1, i 0, p $PapStartupLockOverlapped) i.R0 ?e'
      Pop $R2
      ${If} $R0 != 0
        StrCpy $PapStartupLockHeld 1
        ${ExitDo}
      ${EndIf}
      ${If} $R2 != 33
        !insertmacro PAP_FAIL "Cannot acquire the backend startup lock (Windows error $R2)."
      ${EndIf}
      IntOp $R3 $R3 - 1
      ${If} $R3 == 0
        !insertmacro PAP_FAIL "Another backend startup or update is still in progress. Close Private AI Proxy and retry."
      ${EndIf}
      Sleep 100
    ${Loop}
    DetailPrint "Acquired the Private AI Proxy backend startup lock."
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro PAP_ACQUIRE_STARTUP_LOCK

  SearchPath $R0 "pap.exe"
  ${If} $R0 != ""
    ${StrCase} $R1 "$R0" "L"
    ${StrCase} $R2 "$INSTDIR\pap.exe" "L"
    ${If} $R1 != $R2
      !insertmacro PAP_FAIL "A different pap executable is already on PATH at $R0. Remove that installation before installing ${PRODUCTNAME}."
    ${EndIf}
  ${EndIf}

  ${If} ${FileExists} "$INSTDIR\pap.exe"
    ReadRegStr $R0 SHCTX "${MANUPRODUCTKEY}" ""
    ${StrCase} $R1 "$R0" "L"
    ${StrCase} $R2 "$INSTDIR" "L"
    ${If} $R1 != $R2
      !insertmacro PAP_FAIL "An unrelated pap executable exists at $INSTDIR\pap.exe. Choose another install directory or remove the conflicting file."
    ${EndIf}
    ExecWait '"$INSTDIR\pap.exe" --yes service stop' $R0
    ${If} $R0 != 0
      !insertmacro PAP_FAIL "The existing Private AI Proxy backend could not be stopped. The installation was not replaced."
    ${EndIf}
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ExecWait '"$INSTDIR\pap.exe" cli install' $R0
  ${If} $R0 != 0
    !insertmacro PAP_FAIL "Private AI Proxy was installed, but pap could not be registered in the current user's PATH."
  ${EndIf}
  !insertmacro PAP_RELEASE_STARTUP_LOCK
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro PAP_ACQUIRE_STARTUP_LOCK

  ${If} ${FileExists} "$INSTDIR\pap.exe"
    ExecWait '"$INSTDIR\pap.exe" --yes service stop' $R0
    ${If} $R0 != 0
      !insertmacro PAP_FAIL "The Private AI Proxy backend could not be stopped. Uninstall was cancelled."
    ${EndIf}
    ${If} $UpdateMode != 1
      ExecWait '"$INSTDIR\pap.exe" --yes cli uninstall' $R0
      ${If} $R0 != 0
        !insertmacro PAP_FAIL "pap could not remove its current-user PATH registration. Uninstall was cancelled."
      ${EndIf}
    ${EndIf}
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  !insertmacro PAP_RELEASE_STARTUP_LOCK
!macroend
