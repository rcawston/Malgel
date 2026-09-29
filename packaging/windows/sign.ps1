# Signs a Windows executable with Authenticode and an RFC 3161 timestamp.
#
#   ./packaging/windows/sign.ps1 <file.exe>
#
# Reads SIGNTOOL (path to signtool.exe), CERTIFICATE (path to the .pfx) and
# WINDOWS_CERTIFICATE_PASSWORD from the environment, as set up by the
# release workflow.
param([Parameter(Mandatory)][string]$File)

$ErrorActionPreference = 'Stop'

foreach ($name in 'SIGNTOOL', 'CERTIFICATE', 'WINDOWS_CERTIFICATE_PASSWORD') {
  if (-not [Environment]::GetEnvironmentVariable($name)) { throw "$name is not set" }
}

# Timestamp servers are occasionally unreachable; try a few times.
for ($attempt = 1; ; $attempt++) {
  & $env:SIGNTOOL sign /f $env:CERTIFICATE /p $env:WINDOWS_CERTIFICATE_PASSWORD `
    /fd sha256 /tr http://timestamp.digicert.com /td sha256 /d Malgel $File
  if ($LASTEXITCODE -eq 0) { break }
  if ($attempt -ge 3) { throw "signtool failed with exit code $LASTEXITCODE" }
  Start-Sleep -Seconds 10
}

& $env:SIGNTOOL verify /pa /v $File
if ($LASTEXITCODE -ne 0) { throw "signtool could not verify $File" }
