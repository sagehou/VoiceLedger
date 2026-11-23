[CmdletBinding()]
param(
    [string]$BothDirectory,
    [string]$SystemDirectory
)

$ErrorActionPreference = 'Stop'
$binNs = [long]5000000
$gapCauses = 'endpoint_lost', 'source_discontinuity', 'format_changed', 'system_suspend',
    'queue_overflow', 'writer_failure', 'paused', 'unknown_capture'

function Read-WasapiTrack($Directory, $Source) {
    $tracks = @(Get-ChildItem -LiteralPath $Directory -Directory -Filter "$Source-*")
    if ($tracks.Count -ne 1) { throw "会话目录应包含一条 $Source 轨道。" }
    $rows = Get-Content -LiteralPath (Join-Path $tracks[0].FullName 'timeline.jsonl') -Encoding UTF8 |
        ForEach-Object { if ($_.Trim()) { ConvertFrom-Json -InputObject $_ } }
    [pscustomobject]@{ Directory = $tracks[0].FullName; Rows = @($rows) }
}

function Stop-WasapiAnalysis([string]$Message) {
    $failure = [IO.InvalidDataException]::new($Message)
    $failure.Data['VoiceLedgerEvidence'] = $true
    throw $failure
}

function Read-WaveFormat([IO.BinaryReader]$Reader) {
    if ($Reader.BaseStream.Length -lt 12 -or $Reader.ReadUInt32() -ne 0x46464952) {
        Stop-WasapiAnalysis '需要完整的 RIFF WAV 文件。'
    }
    $null = $Reader.ReadUInt32()
    if ($Reader.ReadUInt32() -ne 0x45564157) { Stop-WasapiAnalysis '文件头未标记为 RIFF WAV。' }
    $stream = $Reader.BaseStream
    $format = $null
    while ($stream.Position + 8 -le $stream.Length) {
        $chunk = [Text.Encoding]::ASCII.GetString($Reader.ReadBytes(4))
        $size = $Reader.ReadUInt32()
        $next = $stream.Position + $size + ($size % 2)
        if ($stream.Position + $size -gt $stream.Length) { Stop-WasapiAnalysis 'WAV 分段尚未完整保存。' }
        if ($chunk -eq 'fmt ' -and $size -ge 16) {
            $format = $Reader.ReadBytes([int][math]::Min($size, 40))
        }
        if ($chunk -eq 'data' -and $null -ne $format) {
            $encoding = [int][BitConverter]::ToUInt16($format, 0)
            if ($encoding -eq 65534 -and $format.Length -ge 40) {
                $encoding = switch ([guid]::new([byte[]]($format[24..39]))) {
                    '00000001-0000-0010-8000-00aa00389b71' { 1 }
                    '00000003-0000-0010-8000-00aa00389b71' { 3 }
                    default { 0 }
                }
            }
            $bits = [int][BitConverter]::ToUInt16($format, 14)
            $channels = [int][BitConverter]::ToUInt16($format, 2)
            $rate = [int][BitConverter]::ToUInt32($format, 4)
            $block = [int][BitConverter]::ToUInt16($format, 12)
            if ($channels -le 0 -or $rate -le 0 -or $block -ne $channels * $bits / 8 -or
                (($encoding -ne 1 -or $bits -notin 8,16,24,32) -and ($encoding -ne 3 -or $bits -ne 32))) {
                Stop-WasapiAnalysis ("暂无法自动匹配该 WAV 格式（编码 {0}，{1} 位，{2} 声道，{3} Hz）。" -f $encoding, $bits, $channels, $rate)
            }
            return [pscustomobject]@{
                Encoding = $encoding; Bits = $bits; Channels = $channels; Rate = $rate
                Block = $block; Start = $stream.Position; Length = $size
            }
        }
        $stream.Position = $next
    }
    Stop-WasapiAnalysis 'WAV 缺少音频数据或格式。'
}

function Get-WasapiEnvelope($Track, [long]$Begin, [int]$Count) {
    $levels = [double[]]::new($Count)
    $samples = [int[]]::new($Count)
    $known = [bool[]]::new($Count)
    $estimated = [bool[]]::new($Count)
    $end = $Begin + $Count * $binNs
    foreach ($row in $Track.Rows) {
        if ($row.kind -eq 'silence') {
            $first = [int][math]::Max(0, [math]::Ceiling(($row.start_ns - $Begin) / $binNs))
            $last = [int][math]::Min($Count, [math]::Floor(($row.end_ns - $Begin) / $binNs))
            for ($bin = $first; $bin -lt $last; $bin++) { $known[$bin] = $true }
        }
        if ($row.kind -ne 'wav_segment' -or $row.start_ns -ge $end -or $row.end_ns -le $Begin) { continue }
        if ($row.filename -notmatch '^\d+-[0-9a-f-]{36}\.wav$') { Stop-WasapiAnalysis '日志中的 WAV 文件名无效。' }
        $wavePath = Join-Path $Track.Directory $row.filename
        if (-not (Test-Path -LiteralPath $wavePath -PathType Leaf)) {
            Stop-WasapiAnalysis '找不到日志登记的 WAV。请使用同时包含日志和 WAV 的完整会话目录。'
        }
        $reader = [IO.BinaryReader]::new([IO.File]::OpenRead($wavePath))
        try {
            $format = Read-WaveFormat $reader
            $stride = [int][math]::Max(1, [math]::Floor($format.Rate / 8000))
            $sampleNs = 1e9 / $format.Rate
            $sampleBytes = [int]($format.Bits / 8)
            foreach ($packet in $row.packets) {
                if ($packet.start_ns -ge $end -or $packet.end_ns -le $Begin) { continue }
                $length = [int]($packet.frames * $format.Block)
                if ($length -le 0 -or $length -gt 1048576 -or $packet.pcm_offset_bytes -lt 0 -or
                    $packet.pcm_offset_bytes + $length -gt $format.Length) {
                    Stop-WasapiAnalysis 'WAV 包索引超出已保存的音频范围。'
                }
                $reader.BaseStream.Position = $format.Start + $packet.pcm_offset_bytes
                $bytes = $reader.ReadBytes($length)
                $offsetNs = $packet.start_ns - $Begin
                for ($frame = 0; $frame -lt $packet.frames; $frame += $stride) {
                    $bin = [int][math]::Floor(($offsetNs + $frame * $sampleNs) / $binNs)
                    if ($bin -lt 0 -or $bin -ge $Count) { continue }
                    for ($channel = 0; $channel -lt $format.Channels; $channel++) {
                        $p = $frame * $format.Block + $channel * $sampleBytes
                        $sample = if ($format.Encoding -eq 3) {
                            [BitConverter]::ToSingle($bytes, $p)
                        } else {
                            switch ($format.Bits) {
                                8  { ($bytes[$p] - 128) / 128.0 }
                                16 { [BitConverter]::ToInt16($bytes, $p) / 32768.0 }
                                24 {
                                    $value = $bytes[$p] + 256 * $bytes[$p + 1] + 65536 * $bytes[$p + 2]
                                    if ($value -ge 8388608) { $value -= 16777216 }
                                    $value / 8388608.0
                                }
                                32 { [BitConverter]::ToInt32($bytes, $p) / 2147483648.0 }
                            }
                        }
                        $levels[$bin] += [double]$sample * $sample
                        $samples[$bin]++
                    }
                    $known[$bin] = $true
                    if ($packet.estimated_timestamp) { $estimated[$bin] = $true }
                }
            }
        } finally {
            $reader.Dispose()
        }
    }
    for ($bin = 0; $bin -lt $Count; $bin++) {
        if ($samples[$bin]) { $levels[$bin] = [math]::Sqrt($levels[$bin] / $samples[$bin]) }
    }
    [pscustomobject]@{ Level = $levels; Known = $known; Estimated = $estimated }
}

function Measure-BothAlignment($MicTrack, $SystemTrack) {
    $firstAudio = $SystemTrack.Rows | Where-Object { $_.kind -eq 'wav_segment' } | Select-Object -First 1
    if ($null -eq $firstAudio) { return }
    $firstAudioNs = [long]$firstAudio.start_ns
    $begin = [math]::Max([long]0, ($firstAudioNs - [long]750000000))
    $end = $firstAudioNs + [long]4000000000
    # The test cue is played before disconnecting either endpoint.
    foreach ($row in ($MicTrack.Rows + $SystemTrack.Rows)) {
        if ($row.kind -eq 'stream_ended') { $end = [math]::Min($end, [long]$row.end_ns) }
    }
    $count = [int][math]::Floor(($end - $begin) / $binNs)
    if ($count -lt 40) { return }

    $reference = Get-WasapiEnvelope $SystemTrack $begin $count
    $capture = Get-WasapiEnvelope $MicTrack $begin $count
    $peak = 0
    for ($bin = 1; $bin -lt $count; $bin++) {
        if ($reference.Level[$bin] -gt $reference.Level[$peak]) { $peak = $bin }
    }
    $first = [math]::Max(0, $peak - 25)
    $last = [math]::Min($count, $first + 100)
    $candidates = for ($shift = -100; $shift -le 100; $shift++) {
        $pairs = 0
        $xx = $yy = $xy = $sx = $sy = 0.0
        $usesEstimate = $false
        for ($i = $first; $i -lt $last; $i++) {
            $j = $i + $shift
            if ($j -lt 0 -or $j -ge $count -or -not $reference.Known[$i] -or -not $capture.Known[$j]) { continue }
            $x = $reference.Level[$i]
            $y = $capture.Level[$j]
            $sx += $x
            $sy += $y
            $xx += $x * $x
            $yy += $y * $y
            $xy += $x * $y
            $pairs++
            $usesEstimate = $usesEstimate -or $reference.Estimated[$i] -or $capture.Estimated[$j]
        }
        if ($pairs -lt 0.8 * ($last - $first)) { continue }
        $varianceX = $xx - $sx * $sx / $pairs
        $varianceY = $yy - $sy * $sy / $pairs
        if ($varianceX -le 1e-6 -or $varianceY -le 1e-6) { continue }
        $score = ($xy - $sx * $sy / $pairs) / [math]::Sqrt($varianceX * $varianceY)
        [pscustomobject]@{ Shift = $shift; Score = $score; Estimated = $usesEstimate }
    }
    $ranked = @($candidates | Sort-Object Score -Descending)
    if ($ranked.Count -eq 0 -or $ranked[0].Score -lt 0.82 -or [math]::Abs($ranked[0].Shift) -eq 100) { return }
    $best = $ranked[0]
    # Nearby offsets are part of the same correlation peak.
    $other = $ranked | Where-Object { [math]::Abs($_.Shift - $best.Shift) -gt 4 } | Select-Object -First 1
    if ($null -ne $other -and $best.Score - $other.Score -lt 0.07) { return }
    [pscustomobject]@{ DeltaMs = $best.Shift * 5; Score = $best.Score; Estimated = $best.Estimated }
}

function Format-WasapiInterval($Row) {
    'epoch {0} / {1:F3}–{2:F3} ms' -f $Row.source_epoch, ($Row.start_ns / 1e6), ($Row.end_ns / 1e6)
}

if (-not $PSBoundParameters.ContainsKey('BothDirectory')) {
    $BothDirectory = Read-Host 'Both 会话目录（含 session.json，回车用当前目录）'
}
if ([string]::IsNullOrWhiteSpace($BothDirectory)) { $BothDirectory = (Get-Location).Path }
$BothDirectory = $BothDirectory.Trim().Trim([char]34)
if (-not (Test-Path -LiteralPath (Join-Path $BothDirectory 'session.json') -PathType Leaf)) {
    throw '请选择停止录音后打开的 Both 会话目录。'
}
if (-not $PSBoundParameters.ContainsKey('SystemDirectory')) {
    $SystemDirectory = Read-Host '独立 System 会话目录（回车用 Both 的 System 轨）'
}
$mic = Read-WasapiTrack $BothDirectory 'mic'
$loopback = Read-WasapiTrack $BothDirectory 'system'
$silenceTrack = $loopback
$silenceLabel = 'Both 的 System 轨'
if (-not [string]::IsNullOrWhiteSpace($SystemDirectory)) {
    $silenceTrack = Read-WasapiTrack ($SystemDirectory.Trim().Trim([char]34)) 'system'
    $silenceLabel = '独立 System 会话'
}

$silence = @($silenceTrack.Rows | Where-Object { $_.kind -eq 'silence' })
$systemGaps = @($silenceTrack.Rows | Where-Object { $_.kind -eq 'gap' })
$lines = @(
    '## Issue #5 Windows x64 真机日志摘要'
    ''
    '- 构建提交 ___ / Windows OS build ___'
    ('- {0} / Silence {1} 条 / 全轨 Gap {2} 条' -f $silenceLabel, $silence.Count, $systemGaps.Count)
)
foreach ($row in $silence) { $lines += ('  - Silence ' + (Format-WasapiInterval $row)) }
foreach ($row in $systemGaps) {
    $cause = if ($row.cause -cin $gapCauses) { $row.cause } else { '未识别原因' }
    $lines += ('  - Gap `{0}` / {1}' -f $cause, (Format-WasapiInterval $row))
}

$tracks = [ordered]@{ 'Mic-A' = $mic; 'Out-A' = $loopback }
foreach ($trackAlias in $tracks.Keys) {
    $rows = $tracks[$trackAlias].Rows
    $gap = @($rows | Where-Object { $_.kind -eq 'gap' -and $_.cause -eq 'endpoint_lost' })
    $epoch = @($rows | Where-Object { $_.kind -eq 'stream_started' } | Select-Object -ExpandProperty source_epoch -Unique)
    $lines += ('- Both {0} / 断连 Gap {1} 条 / 启动 epoch {2}' -f $trackAlias, $gap.Count, ($epoch -join ' → '))
    foreach ($row in $gap) { $lines += ('  - `endpoint_lost` ' + (Format-WasapiInterval $row)) }
}
try {
    $alignment = Measure-BothAlignment $mic $loopback
    if ($null -eq $alignment) {
        $lines += '- Both 对齐待核对。断连前的两轨音频中未找到唯一的测试音匹配。'
    } else {
        $lines += ('- Both 对齐估算 `Mic - System` = {0} ms / 5 ms 分辨率 / 相关系数 {1:F2} / 含估算包时间戳 {2}' -f $alignment.DeltaMs, $alignment.Score, $alignment.Estimated)
    }
} catch {
    $failure = $_.Exception
    if ($failure.Data.Contains('VoiceLedgerEvidence')) {
        $lines += ('- Both 对齐待核对。' + $failure.Message)
    } else {
        $lines += ('- Both 对齐待核对。分析异常（{0}，脚本第 {1} 行）。' -f $failure.GetType().Name, $_.InvocationInfo.ScriptLineNumber)
    }
}
$lines += '- 现场观察与验收状态 ___'
$lines -join [Environment]::NewLine
