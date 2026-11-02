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
            $theseusObserverStatement.Left.Extent.Text -in @('$theseusModelPhases', '$theseusModelSeen')) -or
            ($theseusObserverStatement -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
            $theseusObserverStatement.Name -eq 'Publish-TheseusModelPhases')) {
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
} finally {
    [System.IO.Directory]::Delete($theseusObserverDirectory, $true)
}
