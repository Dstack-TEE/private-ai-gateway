Var PagStartupLockHandle
Var PagStartupLockOverlapped
Var PagStartupLockHeld
Var PagStartupLockPath

!macro PAG_RELEASE_STARTUP_LOCK
  ${If} $PagStartupLockHeld == 1
    System::Call 'kernel32::UnlockFileEx(p $PagStartupLockHandle, i 0, i 1, i 0, p $PagStartupLockOverlapped)'
  ${EndIf}
  ${If} $PagStartupLockHandle != ""
  ${AndIf} $PagStartupLockHandle != -1
    System::Call 'kernel32::CloseHandle(p $PagStartupLockHandle)'
  ${EndIf}
  ${If} $PagStartupLockOverlapped != ""
    System::Free $PagStartupLockOverlapped
  ${EndIf}
  StrCpy $PagStartupLockHandle ""
  StrCpy $PagStartupLockOverlapped ""
  StrCpy $PagStartupLockHeld 0
!macroend

!macro PAG_FAIL MESSAGE
  !insertmacro PAG_RELEASE_STARTUP_LOCK
  DetailPrint "${MESSAGE}"
  ${IfNot} ${Silent}
    MessageBox MB_ICONSTOP|MB_OK "${MESSAGE}"
  ${EndIf}
  SetErrorLevel 1
  Abort
!macroend

!macro PAG_ACQUIRE_STARTUP_LOCK
  ${If} $PagStartupLockHeld != 1
    StrCpy $PagStartupLockHandle ""
    StrCpy $PagStartupLockOverlapped ""
    StrCpy $PagStartupLockHeld 0

    ReadEnvStr $PagStartupLockPath "PRIVATE_AI_GATEWAY_HOME"
    ${If} $PagStartupLockPath == ""
      ReadEnvStr $PagStartupLockPath "APPDATA"
      ${If} $PagStartupLockPath == ""
        !insertmacro PAG_FAIL "APPDATA is not set, so the backend startup lock cannot be located."
      ${EndIf}
      StrCpy $PagStartupLockPath "$PagStartupLockPath\${BUNDLEID}"
    ${Else}
      StrCpy $PagStartupLockPath "$PagStartupLockPath\.private-ai-gateway"
    ${EndIf}
    ClearErrors
    CreateDirectory "$PagStartupLockPath"
    ${If} ${Errors}
      !insertmacro PAG_FAIL "Cannot create the Private AI Gateway data directory at $PagStartupLockPath."
    ${EndIf}
    StrCpy $PagStartupLockPath "$PagStartupLockPath\startup.lock"

    System::Call 'kernel32::CreateFileW(w "$PagStartupLockPath", i 0xC0000000, i 7, p 0, i 4, i 0x80, p 0) p.R0 ?e'
    Pop $R2
    StrCpy $PagStartupLockHandle $R0
    ${If} $PagStartupLockHandle == -1
      StrCpy $PagStartupLockHandle ""
      !insertmacro PAG_FAIL "Cannot open the backend startup lock at $PagStartupLockPath (Windows error $R2)."
    ${EndIf}

    System::Call '*(p 0, p 0, i 0, i 0, p 0) p.R0'
    StrCpy $PagStartupLockOverlapped $R0
    ${If} $PagStartupLockOverlapped == 0
      StrCpy $PagStartupLockOverlapped ""
      !insertmacro PAG_FAIL "Cannot allocate the backend startup lock state."
    ${EndIf}

    StrCpy $R3 200
    ${Do}
      ; Byte zero overlaps Rust File::try_lock's whole-file lock. Never delete the lock file.
      System::Call 'kernel32::LockFileEx(p $PagStartupLockHandle, i 3, i 0, i 1, i 0, p $PagStartupLockOverlapped) i.R0 ?e'
      Pop $R2
      ${If} $R0 != 0
        StrCpy $PagStartupLockHeld 1
        ${ExitDo}
      ${EndIf}
      ${If} $R2 != 33
        !insertmacro PAG_FAIL "Cannot acquire the backend startup lock (Windows error $R2)."
      ${EndIf}
      IntOp $R3 $R3 - 1
      ${If} $R3 == 0
        !insertmacro PAG_FAIL "Another backend startup or update is still in progress. Close Private AI Gateway and retry."
      ${EndIf}
      Sleep 100
    ${Loop}
    DetailPrint "Acquired the Private AI Gateway backend startup lock."
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro PAG_ACQUIRE_STARTUP_LOCK

  SearchPath $R0 "pag.exe"
  ${If} $R0 != ""
    ${StrCase} $R1 "$R0" "L"
    ${StrCase} $R2 "$INSTDIR\pag.exe" "L"
    ${If} $R1 != $R2
      !insertmacro PAG_FAIL "A different pag executable is already on PATH at $R0. Remove that installation before installing ${PRODUCTNAME}."
    ${EndIf}
  ${EndIf}

  ${If} ${FileExists} "$INSTDIR\pag.exe"
    ReadRegStr $R0 SHCTX "${MANUPRODUCTKEY}" ""
    ${StrCase} $R1 "$R0" "L"
    ${StrCase} $R2 "$INSTDIR" "L"
    ${If} $R1 != $R2
      !insertmacro PAG_FAIL "An unrelated pag executable exists at $INSTDIR\pag.exe. Choose another install directory or remove the conflicting file."
    ${EndIf}
    ExecWait '"$INSTDIR\pag.exe" --yes service stop' $R0
    ${If} $R0 != 0
      !insertmacro PAG_FAIL "The existing Private AI Gateway backend could not be stopped. The installation was not replaced."
    ${EndIf}
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ExecWait '"$INSTDIR\pag.exe" cli install' $R0
  ${If} $R0 != 0
    !insertmacro PAG_FAIL "Private AI Gateway was installed, but pag could not be registered in the current user's PATH."
  ${EndIf}
  !insertmacro PAG_RELEASE_STARTUP_LOCK
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro PAG_ACQUIRE_STARTUP_LOCK

  ${If} ${FileExists} "$INSTDIR\pag.exe"
    ExecWait '"$INSTDIR\pag.exe" --yes service stop' $R0
    ${If} $R0 != 0
      !insertmacro PAG_FAIL "The Private AI Gateway backend could not be stopped. Uninstall was cancelled."
    ${EndIf}
    ${If} $UpdateMode != 1
      ExecWait '"$INSTDIR\pag.exe" --yes cli uninstall' $R0
      ${If} $R0 != 0
        !insertmacro PAG_FAIL "pag could not remove its current-user PATH registration. Uninstall was cancelled."
      ${EndIf}
    ${EndIf}
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  !insertmacro PAG_RELEASE_STARTUP_LOCK
!macroend
