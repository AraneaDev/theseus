# CI-only owner/observer for one fixed native test. No native teardown ordering changes.
$ErrorActionPreference = 'Stop'
$theseusModelLog = Join-Path $env:THESEUS_HOST_EVIDENCE 'early-native-conpty-close-model.log'
$theseusModelScript = Join-Path $env:RUNNER_TEMP 'theseus-close-model-observed.cmd'
@(
    '@echo off'
    'cargo test --locked -p theseus-host --features test-support --lib conpty_backpressure_close_model -- --nocapture --test-threads=1 > "%THESEUS_HOST_EVIDENCE%\early-native-conpty-close-model.log" 2>&1'
    'exit /b %ERRORLEVEL%'
) | Set-Content -Encoding ascii $theseusModelScript
$theseusModelPhases = @(
    'native-create-begin', 'native-created', 'native-launch-begin', 'native-launched',
    'input-peer-drop-begin', 'input-peer-drop-returned',
    'output-peer-drop-begin', 'output-peer-drop-returned',
    'pressure-peek-begin', 'pressure-peek-returned', 'pressure-loop-complete', 'reader-spawn-begin',
    'reader-started', 'reader-eof', 'reader-error', 'close-begin',
    'input-close-begin', 'input-close-returned', 'close-worker-start',
    'close-native-begin', 'close-native-complete', 'job-cleanup-begin',
    'job-cleanup-returned', 'close-join-begin', 'close-join-returned',
    'reader-join-begin', 'reader-join-returned',
    'failed-launch-cleanup-begin', 'failed-launch-cleanup-complete'
)
$theseusModelSeen = [System.Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
$theseusModelObserved = [System.Collections.Generic.List[string]]::new()
function Publish-TheseusModelPhases {
    if (-not (Test-Path -LiteralPath $theseusModelLog -PathType Leaf)) { return }
    $theseusModelStream = [System.IO.File]::Open(
        $theseusModelLog, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read,
        ([System.IO.FileShare]::ReadWrite -bor [System.IO.FileShare]::Delete))
    try {
        # The window and whitelist bound all observation/annotation work.
        # A missing/truncated phase is unknown, never fabricated completion.
        $theseusModelBuffer = [byte[]]::new(65536)
        $theseusModelCount = $theseusModelStream.Read($theseusModelBuffer, 0, $theseusModelBuffer.Length)
        $theseusModelText = [System.Text.Encoding]::UTF8.GetString($theseusModelBuffer, 0, $theseusModelCount)
        $theseusModelPattern = '(?m)^(?:test process::windows::terminal::tests::conpty_backpressure_close_model_joins_reader_and_real_native_cleanup \.\.\. )?theseus-terminal pid=[0-9]+ phase=([a-z-]+)\r?\n'
        foreach ($theseusModelMatch in [regex]::Matches($theseusModelText, $theseusModelPattern)) {
            $theseusModelPhase = $theseusModelMatch.Groups[1].Value
            if ($theseusModelPhases -contains $theseusModelPhase) {
                if ($theseusModelSeen.Add($theseusModelPhase)) {
                    $theseusModelObserved.Add($theseusModelPhase)
                    Write-Output "::notice title=Theseus close MODEL phase::$theseusModelPhase"
                }
            }
        }
    } finally {
        $theseusModelStream.Dispose()
    }
}

function New-TheseusModelSnapshot {
    param([string]$SourceSha, [string]$RunId, [string]$RunAttempt, [int]$Ordinal,
        [long]$ElapsedMs, [bool]$LogPresent, [string[]]$Phases = @())
    if ($SourceSha -cnotmatch '\A[0-9a-f]{40}\z' -or $RunId -cnotmatch '\A[1-9][0-9]{0,19}\z' -or
        $RunAttempt -cnotmatch '\A[1-9][0-9]{0,9}\z' -or $Ordinal -notin @(1, 2) -or $ElapsedMs -lt 0) {
        throw 'Invalid fixed MODEL checkpoint metadata'
    }
    if ($Phases.Count -gt 29) { throw 'MODEL checkpoint phase bound exceeded' }
    $theseusSnapshotSeen = [System.Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($theseusSnapshotPhase in $Phases) {
        if ($theseusModelPhases -cnotcontains $theseusSnapshotPhase -or -not $theseusSnapshotSeen.Add($theseusSnapshotPhase)) {
            throw 'Invalid fixed MODEL checkpoint phase'
        }
    }
    $theseusSnapshotJson = [ordered]@{
        schema = 1; source_sha = $SourceSha; run_id = $RunId; run_attempt = $RunAttempt
        checkpoint = $Ordinal; elapsed_ms = $ElapsedMs; log_present = $LogPresent
        native_completion_observed = $false
        phase_status = $(if ($Phases.Count -eq 0) { 'unknown' } else { 'observed' })
        phases = @($Phases)
    } | ConvertTo-Json -Depth 3 -Compress
    if ([System.Text.Encoding]::UTF8.GetByteCount($theseusSnapshotJson) -gt 8192) { throw 'MODEL checkpoint byte bound exceeded' }
    return $theseusSnapshotJson
}
function Get-TheseusModelCheckpoint {
    param([long]$ElapsedMs, [bool]$Returned, [int]$Attempted)
    if ($ElapsedMs -lt 0 -or $Attempted -lt 0 -or $Attempted -gt 2) { throw 'Invalid MODEL checkpoint schedule' }
    if ($Returned -or $Attempted -eq 2) { return 0 }
    if ($Attempted -eq 0 -and $ElapsedMs -ge 15000) { return 1 }
    if ($Attempted -eq 1 -and $ElapsedMs -ge 30000) { return 2 }
    return 0
}
function Write-TheseusModelSnapshot {
    param([string]$Path, [string]$Json)
    $theseusSnapshotBytes = [System.Text.Encoding]::UTF8.GetBytes($Json)
    if ($theseusSnapshotBytes.Length -gt 8192) { throw 'MODEL checkpoint byte bound exceeded' }
    $theseusSnapshotStream = [System.IO.File]::Open($Path, [System.IO.FileMode]::CreateNew,
        [System.IO.FileAccess]::Write, [System.IO.FileShare]::Read)
    try { $theseusSnapshotStream.Write($theseusSnapshotBytes, 0, $theseusSnapshotBytes.Length) }
    finally { $theseusSnapshotStream.Dispose() }
}
function Remove-TheseusModelRuntime {
    param([System.Diagnostics.ProcessStartInfo]$StartInfo)
    # Remove fixed names only; no credential values are read or reported.
    foreach ($theseusRuntimeName in @('ACTIONS_RUNTIME_TOKEN', 'ACTIONS_RUNTIME_URL', 'ACTIONS_RESULTS_URL',
        'ACTIONS_CACHE_URL', 'ACTIONS_ID_TOKEN_REQUEST_TOKEN', 'ACTIONS_ID_TOKEN_REQUEST_URL')) {
        $null = $StartInfo.Environment.Remove($theseusRuntimeName)
    }
}
function Publish-TheseusModelCheckpoint {
    param([int]$Ordinal, [long]$ElapsedMs)
    $theseusSnapshot = New-TheseusModelSnapshot -SourceSha $env:GITHUB_SHA -RunId $env:GITHUB_RUN_ID `
        -RunAttempt $env:GITHUB_RUN_ATTEMPT -Ordinal $Ordinal -ElapsedMs $ElapsedMs `
        -LogPresent (Test-Path -LiteralPath $theseusModelLog -PathType Leaf) -Phases ($theseusModelObserved.ToArray())
    $theseusCheckpointNumber = '{0:00}' -f $Ordinal
    $theseusSnapshotPath = Join-Path $env:THESEUS_HOST_EVIDENCE "model-checkpoint-$theseusCheckpointNumber.json"
    Write-TheseusModelSnapshot $theseusSnapshotPath $theseusSnapshot
    $theseusUploadScript = Join-Path $env:RUNNER_TEMP "theseus-model-checkpoint-$theseusCheckpointNumber.cmd"
    @(
        '@echo off'
        ('"%THESEUS_MODEL_NODE%" "%THESEUS_MODEL_UPLOADER%" > "%THESEUS_HOST_EVIDENCE%\model-checkpoint-' + $theseusCheckpointNumber + '-upload.log" 2>&1')
        'exit /b %ERRORLEVEL%'
    ) | Set-Content -Encoding ascii $theseusUploadScript
    $theseusUploader = [System.Diagnostics.Process]::new()
    $theseusUploader.StartInfo.FileName = $env:ComSpec
    $theseusUploader.StartInfo.UseShellExecute = $false
    $theseusUploader.StartInfo.ArgumentList.Add('/d')
    $theseusUploader.StartInfo.ArgumentList.Add('/c')
    $theseusUploader.StartInfo.ArgumentList.Add($theseusUploadScript)
    # The pinned entrypoint receives explicit action inputs; YAML defaults are not applied here.
    $theseusUploader.StartInfo.Environment['INPUT_NAME'] = "host-package-close-model-checkpoint-$theseusCheckpointNumber-windows-2025"
    $theseusUploader.StartInfo.Environment['INPUT_PATH'] = $theseusSnapshotPath
    $theseusUploader.StartInfo.Environment['INPUT_IF-NO-FILES-FOUND'] = 'error'
    $theseusUploader.StartInfo.Environment['INPUT_RETENTION-DAYS'] = '7'
    $theseusUploader.StartInfo.Environment['INPUT_COMPRESSION-LEVEL'] = '0'
    $theseusUploader.StartInfo.Environment['INPUT_OVERWRITE'] = 'false'
    $theseusUploader.StartInfo.Environment['INPUT_INCLUDE-HIDDEN-FILES'] = 'false'
    $theseusUploader.StartInfo.Environment['INPUT_ARCHIVE'] = 'true'
    $theseusUploaderStarted = $false
    $theseusUploaderExit = $null
    try {
        $theseusUploaderStarted = $theseusUploader.Start()
        if (-not $theseusUploaderStarted) { throw 'Fixed MODEL uploader did not start' }
        $theseusUploader.WaitForExit()
    } finally {
        # A failed upload never releases either actual command owner early.
        if ($theseusUploaderStarted) {
            $theseusUploader.WaitForExit()
            $theseusUploaderExit = $theseusUploader.ExitCode
        }
        $theseusUploader.Dispose()
    }
    if ($theseusUploaderExit -ne 0) { throw 'Fixed MODEL checkpoint upload failed' }
    Write-Output "THESEUS diagnostic=close-model checkpoint=$theseusCheckpointNumber upload=returned"
}

# The local action validates all three pinned files and Node24 before starting us.
if (-not $env:THESEUS_MODEL_NODE -or -not $env:THESEUS_MODEL_UPLOADER) { throw 'Missing fixed MODEL action context' }
$null = New-TheseusModelSnapshot -SourceSha $env:GITHUB_SHA -RunId $env:GITHUB_RUN_ID `
    -RunAttempt $env:GITHUB_RUN_ATTEMPT -Ordinal 1 -ElapsedMs 0 -LogPresent $false -Phases @()
$theseusModelProcess = [System.Diagnostics.Process]::new()
$theseusModelProcess.StartInfo.FileName = $env:ComSpec
$theseusModelProcess.StartInfo.UseShellExecute = $false
$theseusModelProcess.StartInfo.ArgumentList.Add('/d')
$theseusModelProcess.StartInfo.ArgumentList.Add('/c')
$theseusModelProcess.StartInfo.ArgumentList.Add($theseusModelScript)
Remove-TheseusModelRuntime $theseusModelProcess.StartInfo
$theseusModelStarted = $false
$theseusModelExit = $null
$theseusModelObservationFailed = $false
$theseusModelAttempted = 0
$theseusModelClock = [System.Diagnostics.Stopwatch]::new()
Write-Output 'THESEUS diagnostic=close-model phase=start'
try {
    $theseusModelStarted = $theseusModelProcess.Start()
    if (-not $theseusModelStarted) { throw 'Fixed MODEL command did not start' }
    $theseusModelClock.Start()
    do {
        Publish-TheseusModelPhases
        $theseusModelReturned = $theseusModelProcess.WaitForExit(100)
        $theseusModelCheckpoint = Get-TheseusModelCheckpoint $theseusModelClock.ElapsedMilliseconds $theseusModelReturned $theseusModelAttempted
        if ($theseusModelCheckpoint -ne 0) {
            # Refresh immediately before the observation. Exit can occur later;
            # the snapshot records only that completion has not been observed.
            Publish-TheseusModelPhases
            if (-not $theseusModelProcess.WaitForExit(0)) {
                $theseusModelAttempted = $theseusModelCheckpoint
                Publish-TheseusModelCheckpoint $theseusModelCheckpoint $theseusModelClock.ElapsedMilliseconds
            }
        }
    } while (-not $theseusModelReturned)
    Publish-TheseusModelPhases
} catch {
    $theseusModelObservationFailed = $true
    Write-Output '::error title=Theseus close MODEL observer::Owned phase observation failed; native completion remains required'
    @{ exception_type = $_.Exception.GetType().FullName; hresult = $_.Exception.HResult } |
        ConvertTo-Json | Set-Content -Encoding ascii (Join-Path $env:THESEUS_HOST_EVIDENCE 'model-observer-error.json')
} finally {
    # Even observer/read/notice failure cannot orphan the actual command owner.
    if ($theseusModelStarted) {
        $theseusModelProcess.WaitForExit()
        $theseusModelExit = $theseusModelProcess.ExitCode
    }
    $theseusModelProcess.Dispose()
}
if (-not $theseusModelStarted) { throw 'Fixed MODEL command did not start' }
if (-not (Test-Path -LiteralPath $theseusModelLog -PathType Leaf)) { throw 'Missing required MODEL log' }
Get-Content -LiteralPath $theseusModelLog
Write-Output "THESEUS diagnostic=close-model phase=returned exit=$theseusModelExit"
if ($theseusModelExit -ne 0) { exit $theseusModelExit }
if ($theseusModelObservationFailed) { throw 'MODEL observer failed after owned command completion' }
