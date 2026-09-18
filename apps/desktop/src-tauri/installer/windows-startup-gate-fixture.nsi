Unicode true
Name "Private AI Proxy startup gate fixture"
OutFile "${OUTPUT}"
SilentInstall silent
RequestExecutionLevel user

!include "LogicLib.nsh"
!addincludedir "${HOOKS_DIR}"
!include "windows.nsh"

Section
  !insertmacro PAP_ACQUIRE_STARTUP_LOCK
  ReadEnvStr $R4 "PAP_GATE_ACQUIRED"
  ClearErrors
  FileOpen $R5 "$R4" w
  ${If} ${Errors}
    !insertmacro PAP_FAIL "Cannot write the startup gate acquired marker."
  ${EndIf}
  FileWrite $R5 "acquired"
  FileClose $R5

  ReadEnvStr $R4 "PAP_GATE_RELEASE"
  StrCpy $R5 450
pap_gate_wait:
  IfFileExists "$R4" pap_gate_release
  Sleep 100
  IntOp $R5 $R5 - 1
  IntCmp $R5 0 pap_gate_timeout pap_gate_wait pap_gate_wait
pap_gate_timeout:
  !insertmacro PAP_FAIL "Startup gate fixture release timed out."
pap_gate_release:
  !insertmacro PAP_RELEASE_STARTUP_LOCK
SectionEnd
