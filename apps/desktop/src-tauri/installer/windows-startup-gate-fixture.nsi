Unicode true
Name "Private AI Gateway startup gate fixture"
OutFile "${OUTPUT}"
SilentInstall silent
RequestExecutionLevel user

!include "LogicLib.nsh"
!addincludedir "${HOOKS_DIR}"
!include "windows.nsh"

Section
  !insertmacro PAG_ACQUIRE_STARTUP_LOCK
  ReadEnvStr $R4 "PAG_GATE_ACQUIRED"
  ClearErrors
  FileOpen $R5 "$R4" w
  ${If} ${Errors}
    !insertmacro PAG_FAIL "Cannot write the startup gate acquired marker."
  ${EndIf}
  FileWrite $R5 "acquired"
  FileClose $R5

  ReadEnvStr $R4 "PAG_GATE_RELEASE"
  StrCpy $R5 450
pag_gate_wait:
  IfFileExists "$R4" pag_gate_release
  Sleep 100
  IntOp $R5 $R5 - 1
  IntCmp $R5 0 pag_gate_timeout pag_gate_wait pag_gate_wait
pag_gate_timeout:
  !insertmacro PAG_FAIL "Startup gate fixture release timed out."
pag_gate_release:
  !insertmacro PAG_RELEASE_STARTUP_LOCK
SectionEnd
