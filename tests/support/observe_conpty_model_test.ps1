# Exercise the actual fixed observer parser without starting a native owner.
$ErrorActionPreference = 'Stop'
$theseusObserverTokens = $null
$theseusObserverErrors = $null
$theseusObserverAst = [System.Management.Automation.Language.Parser]::ParseFile(
    (Join-Path $PSScriptRoot 'observe_conpty_model.ps1'),
    [ref]$theseusObserverTokens, [ref]$theseusObserverErrors)
if ($theseusObserverErrors.Count -ne 0) { throw 'MODEL observer syntax errors' }
$theseusObserverDirectory = Join-Path ([System.IO.Path]::GetTempPath()) ([guid]::NewGuid().ToString('N'))
[System.IO.Directory]::CreateDirectory($theseusObserverDirectory) | Out-Null
try {
    # Load only the trusted source's whitelist, seen set and actual parser;
    # the fixed Cargo command/Process owner is deliberately not executed here.
    foreach ($theseusObserverStatement in $theseusObserverAst.EndBlock.Statements) {
        if (($theseusObserverStatement -is [System.Management.Automation.Language.AssignmentStatementAst] -and
            $theseusObserverStatement.Left.Extent.Text -in @('$theseusModelPhases', '$theseusModelSeen', '$theseusModelObserved')) -or
            ($theseusObserverStatement -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
            $theseusObserverStatement.Name -in @('Publish-TheseusModelPhases', 'New-TheseusModelSnapshot',
                'Get-TheseusModelCheckpoint', 'Write-TheseusModelSnapshot', 'Remove-TheseusModelRuntime'))) {
            . ([scriptblock]::Create($theseusObserverStatement.Extent.Text))
        }
    }
    $theseusModelLog = Join-Path $theseusObserverDirectory 'model.log'
    $theseusObserverPrefix = 'test process::windows::terminal::tests::conpty_backpressure_close_model_joins_reader_and_real_native_cleanup ... '
    $theseusObserverLines = @(
        "${theseusObserverPrefix}theseus-terminal pid=12 phase=native-created"
        'theseus-terminal pid=12 phase=native-created'
        'theseus-terminal pid=12 phase=unknown-phase'
        'theseus-terminal pid=12 phase=reader-eof ::error::untrusted'
        'untrusted theseus-terminal pid=12 phase=reader-eof'
    ) -join "`n"
    [System.IO.File]::WriteAllText($theseusModelLog, $theseusObserverLines + "`ntheseus-terminal pid=12 phase=reader-eof")
    $theseusObserverFirst = @(Publish-TheseusModelPhases)
    if ($theseusObserverFirst.Count -ne 1 -or $theseusObserverFirst[0] -cne '::notice title=Theseus close MODEL phase::native-created') {
        throw 'MODEL observer did not reject duplicate, incomplete or untrusted phase data'
    }
    [System.IO.File]::AppendAllText($theseusModelLog, "`n")
    $theseusObserverSecond = @(Publish-TheseusModelPhases)
    if ($theseusObserverSecond.Count -ne 1 -or $theseusObserverSecond[0] -cne '::notice title=Theseus close MODEL phase::reader-eof') {
        throw 'MODEL observer did not publish the completed fixed phase exactly once'
    }
    if (@(Publish-TheseusModelPhases).Count -ne 0) { throw 'MODEL observer repeated a phase' }
    [System.IO.File]::WriteAllText($theseusModelLog, ('x' * 65536) + "`ntheseus-terminal pid=12 phase=close-native-complete`n")
    if (@(Publish-TheseusModelPhases).Count -ne 0) { throw 'MODEL observer exceeded its bounded window' }
    @{ result = 'passed'; controls = 4; scope = 'actual-fixed-parser-only' } | ConvertTo-Json |
        Set-Content -Encoding ascii (Join-Path $env:THESEUS_HOST_EVIDENCE 'model-observer-parser-check.json')
    Write-Output 'THESEUS MODEL observer parser controls=4 passed'

    # Pure scheduling/serialization and actual immutable files; no native owner start.
    $theseusSnapshotArguments = @{
        SourceSha = ('a' * 40); RunId = '12345'; RunAttempt = '2'; Ordinal = 1
        ElapsedMs = 15000; LogPresent = $true; Phases = @('native-created', 'native-launched')
    }
    $theseusSnapshot = New-TheseusModelSnapshot @theseusSnapshotArguments
    $theseusSnapshotValue = $theseusSnapshot | ConvertFrom-Json
    if ($theseusSnapshotValue.source_sha -cne ('a' * 40) -or $theseusSnapshotValue.run_id -cne '12345' -or
        $theseusSnapshotValue.run_attempt -cne '2' -or $theseusSnapshotValue.native_completion_observed -ne $false -or
        ($theseusSnapshotValue.phases -join ',') -cne 'native-created,native-launched') {
        throw 'Snapshot lost source identity, phase order or native-completion qualification'
    }
    if (($theseusModelObserved -join ',') -cne 'native-created,reader-eof') {
        throw 'Actual parser lost first-observed ordering'
    }
    $theseusProbePhases = @('input-peer-drop-begin', 'input-peer-drop-returned',
        'output-peer-drop-begin', 'output-peer-drop-returned', 'pressure-peek-begin',
        'pressure-peek-returned', 'pressure-loop-complete', 'reader-spawn-begin')
    [System.IO.File]::WriteAllText($theseusModelLog, (($theseusProbePhases | ForEach-Object {
        "theseus-terminal pid=12 phase=$_`n"
    }) -join ''))
    $theseusProbeNotices = @(Publish-TheseusModelPhases)
    if ($theseusProbeNotices.Count -ne 8 -or
        ($theseusModelObserved.GetRange(2, 8) -join ',') -cne ($theseusProbePhases -join ',')) {
        throw 'Actual parser lost a fixed native boundary probe'
    }
    $theseusAllPhaseArguments = $theseusSnapshotArguments.Clone()
    $theseusAllPhaseArguments.Phases = $theseusModelPhases
    $theseusAllPhases = New-TheseusModelSnapshot @theseusAllPhaseArguments | ConvertFrom-Json
    if (($theseusAllPhases.phases -join ',') -cne ($theseusModelPhases -join ',')) {
        throw 'Snapshot lost a fixed native boundary probe'
    }
    $theseusEmptyArguments = $theseusSnapshotArguments.Clone()
    $theseusEmptyArguments.Phases = @()
    $theseusEmptyArguments.LogPresent = $false
    $theseusEmpty = New-TheseusModelSnapshot @theseusEmptyArguments | ConvertFrom-Json
    if ($theseusEmpty.phase_status -cne 'unknown' -or $theseusEmpty.phases.Count -ne 0 -or $theseusEmpty.log_present) {
        throw 'Missing phase bytes must remain unknown'
    }
    foreach ($theseusInvalidMetadata in @(
        @{ SourceSha = 'bad' }, @{ SourceSha = (('a' * 40) + "`n") },
        @{ RunId = "123`n::error::injected" }, @{ RunId = '0' }, @{ RunAttempt = '-1' },
        @{ Ordinal = 3 }, @{ ElapsedMs = -1 }
    )) {
        $theseusInvalidArguments = $theseusSnapshotArguments.Clone()
        foreach ($theseusKey in $theseusInvalidMetadata.Keys) { $theseusInvalidArguments[$theseusKey] = $theseusInvalidMetadata[$theseusKey] }
        $theseusRejected = $false
        try { $null = New-TheseusModelSnapshot @theseusInvalidArguments } catch { $theseusRejected = $true }
        if (-not $theseusRejected) { throw 'Invalid snapshot metadata was accepted' }
    }
    foreach ($theseusInvalidPhases in @(
        @{ CasePhases = @('unknown-phase') }, @{ CasePhases = @('native-created', 'native-created') },
        @{ CasePhases = @('reader-eof ::error::injected') }, @{ CasePhases = (@('reader-eof') * 30) }
    )) {
        $theseusInvalidArguments = $theseusSnapshotArguments.Clone()
        $theseusInvalidArguments.Phases = $theseusInvalidPhases.CasePhases
        $theseusRejected = $false
        try { $null = New-TheseusModelSnapshot @theseusInvalidArguments } catch { $theseusRejected = $true }
        if (-not $theseusRejected) { throw 'Unbounded or untrusted phases were accepted' }
    }
    foreach ($theseusScheduleCase in @(
        @(2000, $false, 0, 0), @(14999, $false, 0, 0), @(15000, $false, 0, 1),
        @(29999, $false, 1, 0), @(30000, $false, 1, 2), @(30000, $false, 2, 0), @(30000, $true, 0, 0),
        @(30000, $false, 0, 1), @(30000, $false, 1, 2)
    )) {
        if ((Get-TheseusModelCheckpoint $theseusScheduleCase[0] $theseusScheduleCase[1] $theseusScheduleCase[2]) -ne $theseusScheduleCase[3]) {
            throw 'Checkpoint schedule duplicated, skipped or published after return'
        }
    }
    $theseusSnapshotFile = Join-Path $theseusObserverDirectory 'checkpoint.json'
    Write-TheseusModelSnapshot $theseusSnapshotFile $theseusSnapshot
    if ([System.IO.File]::ReadAllText($theseusSnapshotFile) -cne $theseusSnapshot) { throw 'Snapshot bytes changed' }
    $theseusRejected = $false
    try { Write-TheseusModelSnapshot $theseusSnapshotFile '{}' } catch { $theseusRejected = $true }
    if (-not $theseusRejected -or [System.IO.File]::ReadAllText($theseusSnapshotFile) -cne $theseusSnapshot) {
        throw 'Checkpoint publication overwrote immutable evidence'
    }
    $theseusRejected = $false
    try { Write-TheseusModelSnapshot (Join-Path $theseusObserverDirectory 'oversized.json') ('x' * 8193) } catch { $theseusRejected = $true }
    if (-not $theseusRejected -or (Test-Path (Join-Path $theseusObserverDirectory 'oversized.json'))) {
        throw 'Oversized snapshot reached the filesystem'
    }
    $theseusStartInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $theseusStartInfo.Environment.Clear()
    foreach ($theseusRuntimeName in @('ACTIONS_RUNTIME_TOKEN', 'ACTIONS_RUNTIME_URL', 'ACTIONS_RESULTS_URL',
        'ACTIONS_CACHE_URL', 'ACTIONS_ID_TOKEN_REQUEST_TOKEN', 'ACTIONS_ID_TOKEN_REQUEST_URL')) {
        $theseusStartInfo.Environment[$theseusRuntimeName] = 'synthetic-control'
    }
    $theseusStartInfo.Environment['THESEUS_TERMINAL_TRACE'] = '1'
    Remove-TheseusModelRuntime $theseusStartInfo
    if ($theseusStartInfo.Environment.Count -ne 1 -or $theseusStartInfo.Environment['THESEUS_TERMINAL_TRACE'] -cne '1') {
        throw 'Native child retained action runtime fields or lost unrelated environment'
    }
    @{ result = 'passed'; controls = 8; scope = 'actual-checkpoint-helpers-only' } | ConvertTo-Json |
        Set-Content -Encoding ascii (Join-Path $env:THESEUS_HOST_EVIDENCE 'model-observer-checkpoint-check.json')
    Write-Output 'THESEUS MODEL observer checkpoint controls=8 passed'
} finally {
    [System.IO.Directory]::Delete($theseusObserverDirectory, $true)
}
