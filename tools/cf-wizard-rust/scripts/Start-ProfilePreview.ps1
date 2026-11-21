param(
    [string]$Executable = '',
    [switch]$Wait,
    [string[]]$ApplicationArguments = @()
)
$ErrorActionPreference = 'Stop'
if (-not $Executable) {
    $Executable = Join-Path $PSScriptRoot 'CF-Wizard-0.8.10.exe'
}
$resolvedExecutable = (Resolve-Path -LiteralPath $Executable).Path
function Quote-WindowsArgument([string]$Value) {
    $escapedArgument = [regex]::Replace($Value, '(\\*)"', '${1}${1}\"')
    $escapedArgument = [regex]::Replace($escapedArgument, '(\\+)$', '${1}${1}')
    return '"' + $escapedArgument + '"'
}
$launchInfo = New-Object System.Diagnostics.ProcessStartInfo
$launchInfo.FileName = $resolvedExecutable
$launchInfo.WorkingDirectory = Split-Path -Parent $resolvedExecutable
$launchInfo.UseShellExecute = $false
$launchInfo.CreateNoWindow = $true
$launchInfo.EnvironmentVariables['CF_WIZARD_PROFILE'] = '1'
$launchInfo.Arguments = ($ApplicationArguments | ForEach-Object { Quote-WindowsArgument $_ }) -join ' '
$diagnosticProcess = [System.Diagnostics.Process]::Start($launchInfo)
Write-Output ('CF Wizard diagnostics started, PID {0}' -f $diagnosticProcess.Id)
if ($Wait) {
    $diagnosticProcess.WaitForExit()
    exit $diagnosticProcess.ExitCode
}
