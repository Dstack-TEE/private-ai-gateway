!macro PAG_FAIL MESSAGE
  DetailPrint "${MESSAGE}"
  ${IfNot} ${Silent}
    MessageBox MB_ICONSTOP|MB_OK "${MESSAGE}"
  ${EndIf}
  SetErrorLevel 1
  Abort
!macroend

!macro NSIS_HOOK_PREINSTALL
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
!macroend

!macro NSIS_HOOK_PREUNINSTALL
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
!macroend
