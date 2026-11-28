param(
  [Parameter(Mandatory)]
  [string[]] $Path
)

$ErrorActionPreference = 'Stop'
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString().ToLowerInvariant()
$dumpbin = & $vswhere -latest -products '*' -find "VC\Tools\MSVC\**\bin\Host$arch\$arch\dumpbin.exe" | Select-Object -First 1
if (!$dumpbin) { throw "Visual Studio dumpbin.exe for $arch was not found" }

foreach ($entry in $Path) {
  $executables = @(Get-ChildItem -Path $entry -Filter '*.exe' -File)
  if (!$executables.Count) { throw "No executables found at $entry" }
  foreach ($executable in $executables) {
    $imports = & $dumpbin /nologo /dependents $executable.FullName
    if ($LASTEXITCODE -ne 0) { throw "dumpbin failed for $($executable.FullName)" }
    # Reject both direct and delay-loaded imports of the MSVC Redistributable.
    $redistributable = $imports | Select-String '^\s*(?:VCRUNTIME|MSVCP|CONCRT|VCCORLIB)\d+[\w.]*\.dll\s*$'
    if ($redistributable) { throw "$($executable.FullName) imports $($redistributable.Line.Trim())" }
    Write-Host "$($executable.Name) has no MSVC Redistributable imports"
  }
}
