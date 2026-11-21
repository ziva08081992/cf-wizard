[CmdletBinding()]
param(
    [string]$Executable = '',
    [string]$OutputPath = '',
    [string]$DataDirectory = '',
    [string[]]$ApplicationArguments = @(),
    [switch]$NoPhaseKeys,
    [switch]$NoProfileJournal
)

$ErrorActionPreference = 'Stop'
if (-not $Executable) {
    $Executable = Join-Path $PSScriptRoot 'CF-Wizard-0.9.0.exe'
}
$resolvedExe = (Resolve-Path -LiteralPath $Executable).ProviderPath
if (-not [IO.File]::Exists($resolvedExe)) { throw 'Executable must be a file.' }
if ($ApplicationArguments -contains '--data-dir') {
    throw 'Use -DataDirectory to set the application data directory.'
}
if (-not $OutputPath) {
    $root = Join-Path ([IO.Path]::GetTempPath()) 'CF-Wizard'
    [void][IO.Directory]::CreateDirectory($root)
    $name = 'memory-' + [DateTime]::UtcNow.ToString('yyyyMMdd-HHmmss') + '-' + [guid]::NewGuid().ToString('N')
    $OutputPath = Join-Path $root ($name + '.jsonl')
}
$receiptPath = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutputPath)
$phasePath = $receiptPath + '.phases.jsonl'
if ([IO.File]::Exists($receiptPath) -or [IO.File]::Exists($phasePath)) {
    throw 'Choose a new memory receipt path.'
}

if (-not ('CfWizard.MemoryPreview.Sampler' -as [type])) {
    Add-Type -Path (Join-Path $PSScriptRoot 'MemoryPreview.Native.cs')
}
[CfWizard.MemoryPreview.Sampler]::CheckPlatform()

function Quote-WindowsArgument([string]$Value) {
    if ($Value -and $Value -notmatch '[\s"]') { return $Value }
    $escaped = [regex]::Replace($Value, '(\\*)"', '${1}${1}\"')
    $escaped = [regex]::Replace($escaped, '(\\+)$', '${1}${1}')
    return '"' + $escaped + '"'
}

function Utc-Milliseconds {
    return [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
}

function Write-MemoryRow($Row) {
    $writer.WriteLine((ConvertTo-Json -InputObject $Row -Depth 12 -Compress))
}

function Convert-MemoryReading($Reading) {
    if ($null -eq $Reading) { return $null }
    return [ordered]@{
        pid = $Reading.Pid
        parent_pid = $Reading.ParentPid
        creation_filetime_100ns = $Reading.CreationFileTime100ns
        webview2 = $Reading.WebView2
        observed_qpc_ticks = $Reading.ObservedQpcTicks
        private_commit_bytes = $Reading.PrivateCommitBytes
        working_set_bytes = $Reading.WorkingSetBytes
        lifetime_peak_working_set_bytes = $Reading.LifetimePeakWorkingSetBytes
    }
}

function Write-ObserverError([string]$Stage, $ErrorRecord) {
    $state.Errors++
    $errorType = $null
    if ($null -ne $ErrorRecord) { $errorType = $ErrorRecord.Exception.GetType().Name }
    Write-MemoryRow @{
        event = 'observer_error'; stage = $Stage; exception = $errorType
        utc_ms = Utc-Milliseconds; qpc_ticks = [Diagnostics.Stopwatch]::GetTimestamp()
    }
}

function Test-Count($Value) {
    return (($Value -is [int] -or $Value -is [long]) -and $Value -ge 0)
}

function Read-AppPhases {
    if (-not $state.PhaseFeedAvailable) { return }
    try { $lines = $feed.ReadLines() }
    catch {
        $state.PhaseFeedAvailable = $false
        Write-ObserverError 'read_phase_feed' $_
        return
    }
    foreach ($line in $lines) {
        if ($null -eq $line) {
            Write-ObserverError 'phase_line_size_or_encoding' $null
            continue
        }
        try {
            $marker = ConvertFrom-Json -InputObject $line -ErrorAction Stop
            $safeName = $marker.phase -is [string] -and $marker.phase -cmatch '\A[A-Za-z0-9_.-]{1,64}\z'
            if (-not $safeName -or -not (Test-Count $marker.pid) -or $marker.pid -ne $process.Id -or
                -not (Test-Count $marker.preview_ordinal) -or -not (Test-Count $marker.utc_ms) -or
                $marker.utc_ms -lt ($state.CreatedUnixMs - 1000) -or $marker.utc_ms -gt ((Utc-Milliseconds) + 1000)) {
                Write-ObserverError 'phase_identity_or_shape' $null
                continue
            }
            $counts = [ordered]@{}
            if ($marker.counts -is [pscustomobject]) {
                foreach ($field in $marker.counts.PSObject.Properties) {
                    if ($field.Name -cmatch '\A[A-Za-z0-9_.-]{1,64}\z' -and (Test-Count $field.Value)) {
                        $counts[$field.Name] = $field.Value
                    }
                }
            }
            $state.Phase = ('preview_{0}.{1}' -f $marker.preview_ordinal, $marker.phase)
            Write-MemoryRow @{
                event = 'phase'; source = 'application'; pid = $process.Id; phase = $state.Phase
                utc_ms = $marker.utc_ms; observed_qpc_ticks = [Diagnostics.Stopwatch]::GetTimestamp()
                preview_ordinal = $marker.preview_ordinal; counts = $counts
            }
        }
        catch { Write-ObserverError 'phase_json' $_ }
    }
}

function Read-ConsolePhase {
    if (-not $state.PhaseKeys) { return }
    try {
        if (-not [Console]::KeyAvailable) { return }
        $key = [string][Console]::ReadKey($true).KeyChar
    }
    catch { $state.PhaseKeys = $false; return }
    $phase = $phaseKeys[$key]
    if ($phase) {
        $state.Phase = $phase
        Write-MemoryRow @{
            event = 'phase'; source = 'console'; phase = $phase
            utc_ms = Utc-Milliseconds; qpc_ticks = [Diagnostics.Stopwatch]::GetTimestamp()
        }
        Write-Host ('Marked: ' + $phase)
    }
}

$appArgs = @($ApplicationArguments)
if ($DataDirectory) {
    $resolvedData = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($DataDirectory)
    $appArgs += @('--data-dir', $resolvedData)
}
$info = New-Object Diagnostics.ProcessStartInfo
$info.FileName = $resolvedExe
$info.WorkingDirectory = [IO.Path]::GetDirectoryName($resolvedExe)
$info.UseShellExecute = $false
$info.CreateNoWindow = $true
$info.EnvironmentVariables['CF_WIZARD_MEMORY_PHASE_FILE'] = $phasePath
if ($NoProfileJournal) { $info.EnvironmentVariables.Remove('CF_WIZARD_PROFILE') }
else { $info.EnvironmentVariables['CF_WIZARD_PROFILE'] = '1' }
$info.Arguments = ($appArgs | ForEach-Object { Quote-WindowsArgument $_ }) -join ' '
$hasher = [Security.Cryptography.SHA256]::Create()
try {
    $exeStream = [IO.File]::OpenRead($resolvedExe)
    try { $hash = $hasher.ComputeHash($exeStream) }
    finally { $exeStream.Dispose() }
    $exeSha = [BitConverter]::ToString($hash).Replace('-', '').ToLowerInvariant()
}
finally { $hasher.Dispose() }

$phaseKeys = @{
    '1' = 'first_preview'; '2' = 'after_first'; '3' = 'second_preview'; '4' = 'after_second'
    '5' = 'closing'; '6' = 'third_preview'; '7' = 'after_third'
}
$state = @{
    Phase = 'unmarked'; PhaseKeys = -not $NoPhaseKeys -and -not [Console]::IsInputRedirected
    PhaseFeedAvailable = $true; CreatedUnixMs = 0
    Samples = 0; Errors = 0; Skipped = 0; Peaks = @{}
}
$process = $sampler = $phaseStream = $receiptStream = $writer = $null
$exitCode = 1
try {
    $phaseStream = [IO.File]::Open($phasePath, 'CreateNew', 'ReadWrite', 'ReadWrite')
    $receiptStream = [IO.File]::Open($receiptPath, 'CreateNew', 'Write', 'Read')
    $writer = New-Object IO.StreamWriter($receiptStream, (New-Object Text.UTF8Encoding($false)))
    $writer.AutoFlush = $true
    $feed = New-Object CfWizard.MemoryPreview.PhaseFeed($phaseStream)
    $process = [Diagnostics.Process]::Start($info)
    Write-Host ('PID {0}. Memory receipt: {1}' -f $process.Id, $receiptPath)
    if ($state.PhaseKeys) {
        Write-Host 'Console marks: 1/2 first preview/idle, 3/4 second preview/idle, 5 closing, 6/7 third preview/idle.'
    }
    $frequency = [Diagnostics.Stopwatch]::Frequency
    $interval = [long]($frequency / 5)
    $deadline = [Diagnostics.Stopwatch]::GetTimestamp()
    Write-MemoryRow @{
        event = 'start'; schema_version = 1; primary_pid = $process.Id
        executable = [IO.Path]::GetFileName($resolvedExe); executable_sha256 = $exeSha
        qpc_ticks = $deadline; qpc_frequency_hz = $frequency; utc_ms = Utc-Milliseconds
        sample_interval_ms = 200; discovery_interval_ms = 1000
        explicit_data_directory = [bool]$DataDirectory; profile_journal = -not $NoProfileJournal
    }

    try {
        $sampler = New-Object CfWizard.MemoryPreview.Sampler($process)
        $created = $sampler.CreationFileTime100ns
        $state.CreatedUnixMs = [long]($created / 10000) - 11644473600000
        Write-MemoryRow @{ event = 'primary_identity'; pid = $process.Id; creation_filetime_100ns = $created }

        while (-not $process.HasExited) {
            $now = [Diagnostics.Stopwatch]::GetTimestamp()
            if ($now -lt $deadline) {
                $ms = [int][Math]::Ceiling(1000.0 * ($deadline - $now) / $frequency)
                if ($process.WaitForExit($ms)) { break }
            }
            Read-AppPhases
            Read-ConsolePhase
            $sampleStart = [Diagnostics.Stopwatch]::GetTimestamp()
            $sampleUtc = Utc-Milliseconds
            $sample = $sampler.Sample($sampleStart)
            $sampleEnd = [Diagnostics.Stopwatch]::GetTimestamp()
            if ($null -eq $sample) { break }
            foreach ($issue in $sample.Issues) {
                $state.Errors++
                Write-MemoryRow @{
                    event = 'observer_error'; stage = $issue.Stage; pid = $issue.Pid
                    win32_error = $issue.Win32Error; qpc_ticks = $sampleEnd
                }
            }
            $descendants = @($sample.Descendants | ForEach-Object { Convert-MemoryReading $_ })
            $primary = Convert-MemoryReading $sample.Primary
            $group = $sample.ObservedGroupPrivateCommitBytes
            Write-MemoryRow @{
                event = 'sample'; phase = $state.Phase; qpc_ticks = $sampleStart
                end_qpc_ticks = $sampleEnd; utc_ms = $sampleUtc
                discovery_ok = $sample.DiscoveryOk; primary = $primary; descendants = $descendants
                observed_group_private_commit_bytes = $group
            }
            if ($null -ne $primary) {
                $state.Samples++
                if (-not $state.Peaks.ContainsKey($state.Phase)) {
                    $state.Peaks[$state.Phase] = @{
                        primary_samples = 0; group_samples = 0; primary_max_sampled_private_commit_bytes = [uint64]0
                        primary_max_sampled_working_set_bytes = [uint64]0
                        group_max_sampled_observed_private_commit_bytes = $null
                    }
                }
                $peaks = $state.Peaks[$state.Phase]
                $peaks.primary_samples++
                $peaks.primary_max_sampled_private_commit_bytes = [Math]::Max(
                    [uint64]$peaks.primary_max_sampled_private_commit_bytes, [uint64]$primary.private_commit_bytes)
                $peaks.primary_max_sampled_working_set_bytes = [Math]::Max(
                    [uint64]$peaks.primary_max_sampled_working_set_bytes, [uint64]$primary.working_set_bytes)
                if ($null -ne $group) {
                    $peaks.group_samples++
                    $peaks.group_max_sampled_observed_private_commit_bytes = [Math]::Max(
                        [uint64]$peaks.group_max_sampled_observed_private_commit_bytes, [uint64]$group)
                }
            }
            $deadline += $interval
            $now = [Diagnostics.Stopwatch]::GetTimestamp()
            if ($now -gt $deadline) {
                $skipped = [long][Math]::Floor(($now - $deadline) / $interval) + 1
                $state.Skipped += $skipped
                $deadline += $skipped * $interval
            }
        }
    }
    catch { Write-ObserverError 'sampling_stopped' $_ }

    $process.WaitForExit()
    $exitCode = $process.ExitCode
    Read-AppPhases
    if ($state.PhaseFeedAvailable -and $feed.Incomplete) {
        Write-ObserverError 'phase_feed_incomplete' $null
    }
    $coverage = 'sampled'
    if ($state.Samples -eq 0) { $coverage = 'no_samples' }
    elseif ($state.Errors -or $state.Skipped) { $coverage = 'partial' }
    Write-MemoryRow @{
        event = 'end'; exit_code = $exitCode; coverage = $coverage
        valid_primary_samples = $state.Samples; errors = $state.Errors; missed_ticks = $state.Skipped
        utc_ms = Utc-Milliseconds; qpc_ticks = [Diagnostics.Stopwatch]::GetTimestamp()
        phases = $state.Peaks
        descendant_scope = 'active owned ancestry observed by periodic snapshots; shared or short-lived processes may be absent'
    }
    Write-Host ('Memory receipt {0}. Samples: {1}. Application exit: {2}.' -f $coverage, $state.Samples, $exitCode)
}
catch {
    if ($null -eq $process) { throw }
    [Console]::Error.WriteLine('Memory receipt interrupted. Waiting for the application to close.')
    $process.WaitForExit()
    $exitCode = $process.ExitCode
}
finally {
    if ($null -ne $sampler) { $sampler.Dispose() }
    if ($null -ne $process) { $process.Dispose() }
    if ($null -ne $writer) {
        try { $writer.Dispose() } catch { [Console]::Error.WriteLine('Memory receipt could not be flushed.') }
    }
    if ($null -ne $receiptStream) { $receiptStream.Dispose() }
    if ($null -ne $phaseStream) { $phaseStream.Dispose() }
}
exit $exitCode
