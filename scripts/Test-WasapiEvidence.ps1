$ErrorActionPreference = 'Stop'
$tool = Join-Path $PSScriptRoot 'Measure-WasapiEvidence.ps1'
$root = Join-Path ([IO.Path]::GetTempPath()) ('wasapi-evidence-' + [guid]::NewGuid())

function Write-TestWave($Path, [int]$Rate, [int]$CueMs, [bool]$IsFloat) {
    $frames = $Rate
    $sampleBytes = if ($IsFloat) { 4 } else { 2 }
    $fmtSize = if ($IsFloat) { 40 } else { 16 }
    $encoding = if ($IsFloat) { 65534 } else { 1 }
    $writer = [IO.BinaryWriter]::new([IO.File]::Create($Path))
    try {
        $writer.Write([uint32]0x46464952)
        $writer.Write([uint32](20 + $fmtSize + $frames * $sampleBytes))
        $writer.Write([uint32]0x45564157)
        $writer.Write([uint32]0x20746d66)
        $writer.Write([uint32]$fmtSize)
        $writer.Write([uint16]$encoding)
        $writer.Write([uint16]1)
        $writer.Write([uint32]$Rate)
        $writer.Write([uint32]($Rate * $sampleBytes))
        $writer.Write([uint16]$sampleBytes)
        $writer.Write([uint16]($sampleBytes * 8))
        if ($IsFloat) {
            $writer.Write([uint16]22)
            $writer.Write([uint16]32)
            $writer.Write([uint32]4)
            $writer.Write(([guid]'00000003-0000-0010-8000-00aa00389b71').ToByteArray())
        }
        $writer.Write([uint32]0x61746164)
        $writer.Write([uint32]($frames * $sampleBytes))
        for ($frame = 0; $frame -lt $frames; $frame++) {
            $ms = 1000 * $frame / $Rate - $CueMs
            $amplitude = 0.0
            if ($CueMs -ge 0) {
                if ($ms -ge 0 -and $ms -lt 35) { $amplitude = 0.32 }
                if ($ms -ge 65 -and $ms -lt 110) { $amplitude = 0.82 }
                if ($ms -ge 155 -and $ms -lt 180) { $amplitude = 0.33 }
                if ($ms -ge 195 -and $ms -lt 270) { $amplitude = 0.62 }
            }
            $waveSample = $amplitude * [math]::Sin(2 * [math]::PI * 800 * $ms / 1000)
            if ($IsFloat) { $writer.Write([single]$waveSample) }
            else { $writer.Write([int16]($waveSample * 32767)) }
        }
    } finally {
        $writer.Dispose()
    }
}

function Write-TestTrack($Directory, [int]$Rate, [long]$StartNs, [bool]$LoseEndpoint) {
    $null = New-Item -ItemType Directory -Path $Directory
    $filename = '000000-' + [guid]::NewGuid() + '.wav'
    $endNs = $StartNs + 1000000000
    $rows = @(
        @{ kind = 'track'; endpoint_name = 'PRIVATE_TEST_ENDPOINT' }
        @{ kind = 'stream_started'; source_epoch = 0; start_ns = $StartNs }
        @{ kind = 'wav_segment'; start_ns = $StartNs; end_ns = $endNs; filename = $filename; packets = @(
                @{ start_ns = $StartNs; end_ns = $endNs; pcm_offset_bytes = 0; frames = $Rate; estimated_timestamp = $LoseEndpoint }
            ) }
        @{ kind = 'stream_ended'; source_epoch = 0; end_ns = $endNs }
    )
    if ($LoseEndpoint) {
        $rows += @{ kind = 'gap'; cause = 'endpoint_lost'; source_epoch = 0; start_ns = $endNs; end_ns = $endNs + 250000000 }
        $rows += @{ kind = 'stream_started'; source_epoch = 1; start_ns = $endNs + 250000000 }
    }
    $rows | ForEach-Object { ConvertTo-Json -InputObject $_ -Depth 4 -Compress } |
        Set-Content -LiteralPath (Join-Path $Directory 'timeline.jsonl') -Encoding UTF8
    Join-Path $Directory $filename
}

try {
    $both = Join-Path $root 'both'
    $system = Join-Path $root 'system'
    $null = New-Item -ItemType Directory -Path $both, $system
    '{}' | Set-Content -LiteralPath (Join-Path $both 'session.json')
    $systemTrack = Join-Path $system 'system-test'
    $null = New-Item -ItemType Directory -Path $systemTrack
    '{"kind":"silence","source_epoch":0,"start_ns":1000000000,"end_ns":6000000000}' |
        Set-Content -LiteralPath (Join-Path $systemTrack 'timeline.jsonl') -Encoding UTF8

    $micWav = Write-TestTrack (Join-Path $both 'mic-test') 16000 15000000 $true
    $systemWav = Write-TestTrack (Join-Path $both 'system-test') 48000 50000000 $false
    Write-TestWave $micWav 16000 320 $true
    Write-TestWave $systemWav 48000 250 $false
    $report = & $tool -BothDirectory $both -SystemDirectory $system
    if ($report -notmatch ' = 35 ms' -or $report -notmatch 'True' -or
        $report -notmatch 'Silence 1' -or $report -notmatch 'endpoint_lost') {
        throw "Expected a 35 ms match with the estimated timestamp, Silence, and disconnect evidence.`n$report"
    }
    if ($report.Contains($root) -or $report.Contains([IO.Path]::GetFileName($micWav)) -or $report.Contains('PRIVATE_TEST_ENDPOINT')) {
        throw 'The summary exposes recording identifiers.'
    }
    Write-TestWave $micWav 16000 -1 $true
    $report = & $tool -BothDirectory $both -SystemDirectory $system
    if ($report -match ' = [+-]?\d+ ms' -or $report -notmatch 'Silence 1' -or $report -notmatch 'endpoint_lost') {
        throw 'An absent cue must leave the alignment pending and preserve the interval evidence.'
    }
    'WASAPI evidence matched a 35 ms shift across PCM and extensible float recordings and preserved evidence when the cue was absent.'
} finally {
    if (Test-Path -LiteralPath $root) { Remove-Item -LiteralPath $root -Recurse -Force }
}
