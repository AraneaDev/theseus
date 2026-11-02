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
    'reader-started', 'reader-eof', 'reader-error', 'close-begin',
    'input-close-begin', 'input-close-returned', 'close-worker-start',
    'close-native-begin', 'close-native-complete', 'job-cleanup-begin',
    'job-cleanup-returned', 'close-join-begin', 'close-join-returned',
    'reader-join-begin', 'reader-join-returned',
    'failed-launch-cleanup-begin', 'failed-launch-cleanup-complete'
)
$theseusModelSeen = [System.Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
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
                    Write-Output "::notice title=Theseus close MODEL phase::$theseusModelPhase"
                }
            }
        }
    } finally {
        $theseusModelStream.Dispose()
    }
}

$theseusModelProcess = [System.Diagnostics.Process]::new()
$theseusModelProcess.StartInfo.FileName = $env:ComSpec
$theseusModelProcess.StartInfo.UseShellExecute = $false
$theseusModelProcess.StartInfo.ArgumentList.Add('/d')
$theseusModelProcess.StartInfo.ArgumentList.Add('/c')
$theseusModelProcess.StartInfo.ArgumentList.Add($theseusModelScript)
$theseusModelStarted = $false
$theseusModelExit = $null
$theseusModelObservationFailed = $false
Write-Output 'THESEUS diagnostic=close-model phase=start'
try {
    $theseusModelStarted = $theseusModelProcess.Start()
    if (-not $theseusModelStarted) { throw 'Fixed MODEL command did not start' }
    do {
        Publish-TheseusModelPhases
    } while (-not $theseusModelProcess.WaitForExit(100))
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
