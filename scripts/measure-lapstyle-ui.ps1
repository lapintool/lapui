param(
    [Parameter(Mandatory)][string]$Address,
    [ValidateRange(1, 30)][int]$IdleSeconds = 5,
    [switch]$Exercise,
    [string[]]$NavigationControls = @('Button', 'Dropdown', 'Slider', 'Menu', 'Table', 'Input'),
    [string]$OutputPath
)

$ErrorActionPreference = 'Stop'
if ($Address -notmatch '^127\.0\.0\.1:(\d+)$') {
    throw 'Use the loopback LAPUI_CONTROL address printed by Lapui.'
}
$controlPort = [int]$Matches[1]
$connection = Get-NetTCPConnection -LocalAddress 127.0.0.1 -LocalPort $controlPort -State Listen
$appProcess = Get-Process -Id $connection.OwningProcess

function Invoke-Control($Command) {
    $client = [Net.Sockets.TcpClient]::new()
    try {
        $client.Connect('127.0.0.1', $controlPort)
        $stream = $client.GetStream()
        $stream.ReadTimeout = 15000
        $stream.WriteTimeout = 15000
        $writer = [IO.StreamWriter]::new($stream, [Text.UTF8Encoding]::new($false))
        $reader = [IO.StreamReader]::new($stream)
        $writer.WriteLine(($Command | ConvertTo-Json -Compress -Depth 10))
        $writer.Flush()
        $reply = $reader.ReadLine() | ConvertFrom-Json
        if (-not $reply.ok) { throw ($reply.error | ConvertTo-Json -Compress) }
        return $reply.observation
    } finally { $client.Dispose() }
}

function Read-Trace([long]$After) {
    $records = [Collections.Generic.List[object]]::new()
    $until = $null
    $script:traceComplete = $true
    do {
        $page = Invoke-Control @{method='debugTrace.read'; documentEpoch=$documentEpoch; afterSequence=$After; limit=128}
        if ($null -eq $until) { $until = [long]$page.latestSequence }
        foreach ($record in $page.records) { if ([long]$record.sequence -le $until) { $records.Add($record) } }
        if ($page.resyncRequired) {
            $script:traceComplete = $false
            Write-Warning 'Trace retention limit reached; frame counts may be incomplete.'
        }
        $After = [long]$page.nextSequence
    } while ($After -lt $until)
    return $records.ToArray()
}

function Wait-Rendered([long]$Revision) {
    $wait = [Diagnostics.Stopwatch]::StartNew()
    do {
        try {
            $status = Invoke-Control @{method='waitForRender'; documentEpoch=$documentEpoch;
                afterRevision=$Revision; timeoutMs=4000; waitId=[guid]::NewGuid().ToString()}
            if ($status.status -eq 'rendered') { return }
            if ($status.status -ne 'timed_out') { throw "Renderer unavailable: $($status.status)" }
        } catch {
            # Only retry the read-only wait when an old/slow renderer blocks the
            # UI thread beyond the server's one-second observation timeout.
            if ($_ -notmatch 'control_timeout') { throw }
        }
        Start-Sleep -Milliseconds 10
    } while ($wait.Elapsed.TotalSeconds -lt 30)
    throw 'Render did not complete within 30 seconds.'
}

$documentEpoch = (Invoke-Control @{method='diagnostics'}).documentEpoch
$originalTrace = Invoke-Control @{method='debugTrace.read'; documentEpoch=$documentEpoch; limit=1}
try {
    Invoke-Control @{method='debugTrace.configure'; documentEpoch=$documentEpoch; enabled=$true} | Out-Null
    # Let startup/font/icon work and the trace-configuration frame settle.
    Start-Sleep -Seconds 3
    $cursor = (Invoke-Control @{method='debugTrace.read'; documentEpoch=$documentEpoch; limit=1}).latestSequence
    $appProcess.Refresh()
    $cpuStart = $appProcess.TotalProcessorTime.TotalSeconds
    $idleClock = [Diagnostics.Stopwatch]::StartNew()
    Start-Sleep -Seconds $IdleSeconds
    $appProcess.Refresh()
    $cpuSeconds = $appProcess.TotalProcessorTime.TotalSeconds - $cpuStart
    $elapsed = $idleClock.Elapsed.TotalSeconds
    $idleRecords = @(Read-Trace $cursor)
    $idleTraceComplete = $script:traceComplete
    $idleFrames = @($idleRecords | Where-Object { $_.kind -eq 'frame' -and $_.phase -eq 'end' })
    $samples = [Collections.Generic.List[object]]::new()

    if ($Exercise) {
        $controls = Invoke-Control @{method='controls'}
        # Navigate through real Vue components using the app's semantic controls.
        foreach ($name in $NavigationControls) {
            $target = $controls.controls | Where-Object { $_.name -eq $name -and $_.tag -eq 'a' } | Select-Object -First 1
            if (-not $target) { throw "Missing Lapstyle navigation control: $name (use English language)." }
            $cursor = (Invoke-Control @{method='debugTrace.read'; documentEpoch=$documentEpoch; limit=1}).latestSequence
            $clock = [Diagnostics.Stopwatch]::StartNew()
            $operation = Invoke-Control @{method='activate'; documentEpoch=$documentEpoch; ref=$target.ref}
            Wait-Rendered $operation.renderRevision
            $latency = $clock.Elapsed.TotalMilliseconds
            $records = @(Read-Trace $cursor)
            $frames = @($records | Where-Object { $_.kind -eq 'frame' -and $_.phase -eq 'end' })
            $samples.Add([ordered]@{
                control=$name
                requestToRendererMillis=[math]::Round($latency, 2)
                frames=$frames.Count
                traceComplete=$script:traceComplete
                frameMillis=@($frames | ForEach-Object { [math]::Round($_.data.durationMillis, 2) })
                rendererMillis=@($frames | ForEach-Object { [math]::Round($_.data.rendererCallMillis, 2) })
                sceneMillis=@($frames | ForEach-Object { [math]::Round($_.data.sceneBuildMillis, 2) })
                callbackMillis=@($frames | ForEach-Object { [math]::Round($_.data.callbackPhaseMillis, 2) })
                viewport=@($frames | ForEach-Object { [ordered]@{width=$_.data.width; height=$_.data.height} })
                retainedLayoutReads=@($records | Where-Object { $_.kind -eq 'layout_read' -and $_.phase -eq 'end' }).Count
            })
            $controls = $operation
        }
    }
    $diagnostics = Invoke-Control @{method='diagnostics'}
    $report = [ordered]@{
        processId=$appProcess.Id
        documentEpoch=$documentEpoch
        boundary='renderer_returned (does not confirm physical display presentation)'
        idle=[ordered]@{
            elapsedSeconds=[math]::Round($elapsed, 3)
            cpuSeconds=[math]::Round($cpuSeconds, 4)
            cpuPercent=[math]::Round(100 * $cpuSeconds / $elapsed / [Environment]::ProcessorCount, 3)
            completedFrames=$idleFrames.Count
            traceComplete=$idleTraceComplete
        }
        interactions=$samples.ToArray()
        scriptStatus=$diagnostics.scriptStatus
        errors=$diagnostics.errors
    }
    $json = $report | ConvertTo-Json -Depth 12
    if ($OutputPath) { [IO.File]::WriteAllText([IO.Path]::GetFullPath($OutputPath), $json, [Text.UTF8Encoding]::new($false)) }
    $json
} finally {
    if (-not $originalTrace.enabled) {
        Invoke-Control @{method='debugTrace.configure'; documentEpoch=$documentEpoch; enabled=$false} | Out-Null
    }
}
