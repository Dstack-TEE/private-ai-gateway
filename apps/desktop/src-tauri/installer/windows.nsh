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
  ${AndIf} $PassiveMode != 1
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

!macro PAP_CHECK_CLI NAME
  SearchPath $R0 "${NAME}"
  ${If} $R0 != ""
    ${StrCase} $R1 "$R0" "L"
    ${StrCase} $R2 "$INSTDIR\${NAME}" "L"
    ${If} $R1 != $R2
      !insertmacro PAP_FAIL "A different ${NAME} executable is already on PATH at $R0. Remove that installation before installing ${PRODUCTNAME}."
    ${EndIf}
  ${EndIf}
  ${If} ${FileExists} "$INSTDIR\${NAME}"
    ReadRegStr $R0 SHCTX "${MANUPRODUCTKEY}" ""
    ${StrCase} $R1 "$R0" "L"
    ${StrCase} $R2 "$INSTDIR" "L"
    ${If} $R1 != $R2
      !insertmacro PAP_FAIL "An unrelated ${NAME} executable exists at $INSTDIR\${NAME}. Choose another install directory or remove the conflicting file."
    ${EndIf}
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro PAP_ACQUIRE_STARTUP_LOCK
  !insertmacro PAP_CHECK_CLI "private-ai-proxy.exe"
  SearchPath $R0 "pap.exe"
  ${If} $R0 != ""
    !insertmacro PAP_FAIL "A pap.exe executable shadows the pap shortcut at $R0. Remove the conflicting executable before installing ${PRODUCTNAME}."
  ${EndIf}
  ${If} ${FileExists} "$INSTDIR\pap.exe"
    !insertmacro PAP_FAIL "A pap.exe executable shadows the pap shortcut in the install directory. Remove the conflicting executable or choose another directory."
  ${EndIf}
  !insertmacro PAP_CHECK_CLI "pap.cmd"
  SearchPath $R0 "aci.exe"
  ${If} $R0 != ""
    !insertmacro PAP_FAIL "An aci.exe executable shadows the aci shortcut at $R0. Remove the conflicting executable before installing ${PRODUCTNAME}."
  ${EndIf}
  ${If} ${FileExists} "$INSTDIR\aci.exe"
    !insertmacro PAP_FAIL "An aci.exe executable shadows the aci shortcut in the install directory. Remove the conflicting executable or choose another directory."
  ${EndIf}
  !insertmacro PAP_CHECK_CLI "aci.cmd"
  StrCpy $R4 "$INSTDIR\private-ai-proxy.exe"
  ${If} ${FileExists} "$R4"
    ExecWait '"$R4" --yes service stop' $R0
    ${If} $R0 != 0
      !insertmacro PAP_FAIL "The existing Private AI Proxy backend could not be stopped. The installation was not replaced."
    ${EndIf}
  ${EndIf}
!macroend

; sizeof(STARTUPINFOW) depends on the NSIS stub, not the bundled app's architecture.
!if ${NSIS_PTR_SIZE} == 8
  !define PAP_STARTUP_INFO_SIZE 104
!else
  !define PAP_STARTUP_INFO_SIZE 68
!endif

Function PapRegisterCli
  Push $R1
  Push $R2
  Push $R3
  Push $R4
  Push $R5

  ; LPTR allocates a zeroed STARTUPINFOW. The other struct is PROCESS_INFORMATION.
  System::Call 'kernel32::LocalAlloc(i 0x40, p ${PAP_STARTUP_INFO_SIZE}) p.R1'
  System::Call '*(p 0, p 0, i 0, i 0) p.R2'
  StrCpy $R0 "could not allocate process state"
  ${If} $R1 != 0
  ${AndIf} $R2 != 0
    System::Call '*$R1(i ${PAP_STARTUP_INFO_SIZE})'
    StrCpy $R3 '"$INSTDIR\private-ai-proxy.exe" cli install'

    ; ExecWait uses CREATE_DEFAULT_ERROR_MODE, which allows loader dialogs.
    ; Inherit SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX instead.
    System::Call 'kernel32::GetErrorMode() i.R4'
    IntOp $R5 $R4 | 3
    System::Call 'kernel32::SetErrorMode(i R5)'
    System::Call 'kernel32::CreateProcessW(w "$INSTDIR\private-ai-proxy.exe", w R3, p 0, p 0, i 0, i 0x08000000, p 0, w "$INSTDIR", p R1, p R2) i.R3 ?e'
    Pop $R5
    System::Call 'kernel32::SetErrorMode(i R4)'
    ${If} $R3 == 0
      StrCpy $R0 "could not start (Windows error $R5)"
    ${Else}
      System::Call '*$R2(p .R3, p .R4)'
      System::Call 'kernel32::CloseHandle(p R4)'
      ; Use a wall-clock deadline independent of any output from the CLI.
      System::Call 'kernel32::WaitForSingleObject(p R3, i 30000) i.R5'
      ${If} $R5 == 0
        System::Call 'kernel32::GetExitCodeProcess(p R3, *i .R4) i.R5'
        ${If} $R5 == 0
          StrCpy $R0 "could not read its exit code"
        ${ElseIf} $R4 == 0
          StrCpy $R0 0
        ${Else}
          StrCpy $R0 "exit code $R4"
        ${EndIf}
      ${Else}
        StrCpy $R0 "did not finish (wait result $R5)"
        System::Call 'kernel32::TerminateProcess(p R3, i 1) i.R5'
        ${If} $R5 != 0
          System::Call 'kernel32::WaitForSingleObject(p R3, i 5000)'
        ${EndIf}
      ${EndIf}
      System::Call 'kernel32::CloseHandle(p R3)'
    ${EndIf}
  ${EndIf}
  ${If} $R1 != 0
    System::Call 'kernel32::LocalFree(p R1)'
  ${EndIf}
  ${If} $R2 != 0
    System::Free $R2
  ${EndIf}

  Pop $R5
  Pop $R4
  Pop $R3
  Pop $R2
  Pop $R1
FunctionEnd

!macro NSIS_HOOK_POSTINSTALL
  Call PapRegisterCli
  ${If} $R0 == 0
    ${IfNot} ${FileExists} "$INSTDIR\pap.cmd"
      StrCpy $R0 "the pap shortcut is missing"
    ${ElseIfNot} ${FileExists} "$INSTDIR\aci.cmd"
      StrCpy $R0 "the aci shortcut is missing"
    ${EndIf}
  ${EndIf}
  !insertmacro PAP_RELEASE_STARTUP_LOCK

  ; Tauri has already installed the app. A CLI warning must allow it to finish and relaunch.
  ${If} $R0 != 0
    DetailPrint "Private AI Proxy was installed, but could not register command-line access ($R0)."
    DetailPrint 'To retry in PowerShell, run: & "$INSTDIR\private-ai-proxy.exe" cli install'
    ${IfNot} ${Silent}
    ${AndIf} $PassiveMode != 1
      SetDetailsView show
    ${EndIf}
    ClearErrors
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro PAP_ACQUIRE_STARTUP_LOCK

  StrCpy $R4 "$INSTDIR\private-ai-proxy.exe"
  ${If} ${FileExists} "$R4"
    ${If} $UpdateMode <> 1
      ExecWait '"$R4" --yes service disable --for-uninstall' $R0
      ${If} $R0 != 0
        !insertmacro PAP_FAIL "The Private AI Proxy login service could not be disabled. Uninstall was cancelled."
      ${EndIf}
    ${EndIf}
    ExecWait '"$R4" --yes service stop' $R0
    ${If} $R0 != 0
      !insertmacro PAP_FAIL "The Private AI Proxy backend could not be stopped. Uninstall was cancelled."
    ${EndIf}
    ; A backend that was not running (an update never relaunched, a crash, a
    ; sign-out) left agents pointed at the Local API; restore them as stopping
    ; it would have. In-app updates (/UPDATE) keep agents on the Local API:
    ; Tauri's installer skips the uninstaller for them, and this guard keeps
    ; that true if it ever runs it. A manual reinstall that uninstalls first
    ; restores them, as the service stop above does.
    ${If} $UpdateMode <> 1
      ; The reason goes to the uninstall details.
      nsExec::ExecToLog '"$R4" --yes stop --offline --for-uninstall'
      Pop $R0
      ${If} $R0 != 0
        ; The restore can keep failing (a damaged agent config or restore
        ; record, a pending 0.1 import), so it does not block uninstalling;
        ; its data stays unless the user asked to delete it.
        ${If} $DeleteAppDataCheckboxState = 1
          !insertmacro PAP_FAIL "Your coding agents could not be restored: see details. Uninstall was cancelled because deleting the app data would also delete the original agent settings and keys needed to restore them. Fix the problem shown in the details and retry, or uninstall without deleting the app data."
        ${EndIf}
        DetailPrint "Your coding agents could not be restored. The restore data stays in the app data folder; after reinstalling, pap stop --offline --for-uninstall finishes it once the problem above is fixed."
        ${IfNot} ${Silent}
          ${IfNot} ${Cmd} `MessageBox MB_YESNO|MB_ICONEXCLAMATION "Your coding agents could not be restored: see details. Uninstall anyway?$\n$\nThe restore data stays in %APPDATA%, and running pap stop --offline --for-uninstall after reinstalling finishes it once the problem in the details is fixed." IDYES`
            !insertmacro PAP_FAIL "Uninstall was cancelled. Your coding agents were not restored: see details."
          ${EndIf}
        ${EndIf}
      ${EndIf}
    ${EndIf}
    ; Remove the owned alias on upgrades too, before replacing the canonical executable.
    ExecWait '"$R4" --yes cli uninstall' $R0
    ${If} $R0 != 0
      !insertmacro PAP_FAIL "Private AI Proxy could not remove its CLI registration. Uninstall was cancelled."
    ${EndIf}
  ${EndIf}
  ${If} $UpdateMode <> 1
    ; auto-launch uses the app's product name for its per-user Run value.
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "${PRODUCTNAME}"
    ClearErrors
    ReadRegStr $R0 HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "${PRODUCTNAME}"
    ${IfNot} ${Errors}
      !insertmacro PAP_FAIL "The Private AI Proxy Open at Login entry could not be removed. Uninstall was cancelled."
    ${EndIf}
    ClearErrors
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  !insertmacro PAP_RELEASE_STARTUP_LOCK
!macroend
