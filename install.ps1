<#
.SYNOPSIS
  parakeet-transcriber installer for Windows x64.

.DESCRIPTION
  Downloads the Vulkan runtime (GitHub Releases) and the Parakeet TDT 0.6B v3 model
  (+ optional diarization model) from Hugging Face, installs the skill into every
  AI-agent skills directory it can find, writes per-install config.json files, and
  optionally runs a quick smoke test (TTS sample -> real transcription).

.EXAMPLE
  # one-liner (installs with defaults)
  irm https://raw.githubusercontent.com/moesuito/parakeet-transcriber/main/install.ps1 | iex

.EXAMPLE
  # manual, with flags
  .\install.ps1 -SkipDiarization -Test

.EXAMPLE
  # reuse existing binaries/models elsewhere on the machine
  .\install.ps1 -RuntimeDir C:\AI\stt\bin\master -ModelsDir C:\AI\stt\models -SkipRuntime -SkipModels -Test
#>
param(
    [string]$InstallDir = (Join-Path $env:USERPROFILE ".parakeet-transcriber"),
    [string]$RuntimeDir = "",
    [string]$ModelsDir = "",
    [switch]$SkipRuntime,
    [switch]$SkipModels,
    [switch]$SkipDiarization,
    [switch]$SkipAgents,
    [switch]$SkipSkill,
    [string[]]$Agents = @(),
    [switch]$DryRun,
    [switch]$Force,
    [switch]$Test
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$Repo    = "moesuito/parakeet-transcriber"
$RunUrl  = "https://github.com/$Repo/releases/latest/download/parakeet-transcriber-win-x64-vulkan.zip"
$ZipSrc  = "https://github.com/$Repo/archive/refs/heads/main.zip"
$HfBase  = "https://huggingface.co/mudler/parakeet-cpp-gguf/resolve/main"

if (-not $RuntimeDir) { $RuntimeDir = Join-Path $InstallDir "bin" }
if (-not $ModelsDir)  { $ModelsDir  = Join-Path $InstallDir "models" }
$SkillHome = Join-Path $InstallDir "skill"
$canonicalSkill = Join-Path $SkillHome "parakeet-transcriber"

function Say([string]$m)  { Write-Host "[parakeet] $m" }
function Step([string]$m) { Write-Host ""; Write-Host "== $m" }

function Download([string]$url, [string]$dest) {
    if ((Test-Path $dest) -and -not $Force) { Say "cached: $dest"; return $true }
    if ($DryRun) { Say "would download: $url"; Say "            -> $dest"; return $true }
    $dir = Split-Path $dest -Parent
    New-Item -ItemType Directory -Force $dir | Out-Null
    $tmp = "$dest.part"
    try {
        $curl = Get-Command curl.exe -ErrorAction SilentlyContinue
        if ($curl) {
            & $curl.Source -L --fail --silent --show-error --retry 3 -o $tmp $url
            if ($LASTEXITCODE -ne 0) { throw "curl failed with exit code $LASTEXITCODE" }
        } else {
            if ($PSVersionTable.PSVersion.Major -lt 6) {
                Invoke-WebRequest -Uri $url -OutFile $tmp -UseBasicParsing
            } else {
                Invoke-WebRequest -Uri $url -OutFile $tmp
            }
        }
        Move-Item -Force $tmp $dest
        Say "downloaded: $dest"
        return $true
    } catch {
        Remove-Item -Force $tmp -ErrorAction SilentlyContinue
        Say "DOWNLOAD FAILED: $url"
        Say "  $($_.Exception.Message)"
        return $false
    }
}

function Write-SkillConfig([string]$scriptsDir) {
    $cfg = [ordered]@{
        cli         = (Join-Path $RuntimeDir "parakeet-cli.exe")
        diarize_exe = (Join-Path $RuntimeDir "diarize.exe")
        models      = [ordered]@{
            f16         = (Join-Path $ModelsDir "tdt-0.6b-v3-f16.gguf")
            q8_0        = (Join-Path $ModelsDir "tdt-0.6b-v3-q8_0.gguf")
            diarization = (Join-Path $ModelsDir "nemotron-3-diarization-f16.gguf")
        }
        defaults    = [ordered]@{ quant = "f16"; diarize = $true; formats = @("json", "srt", "txt") }
    }
    if ($DryRun) { Say "would write config: $(Join-Path $scriptsDir 'config.json')"; return }
    New-Item -ItemType Directory -Force $scriptsDir | Out-Null
    [System.IO.File]::WriteAllText((Join-Path $scriptsDir "config.json"), ($cfg | ConvertTo-Json -Depth 5), (New-Object System.Text.UTF8Encoding($false)))
}

function Install-SkillInto([string]$targetRoot, [string]$skillSource) {
    $dest = Join-Path $targetRoot "parakeet-transcriber"
    $exists = Test-Path (Join-Path $dest "SKILL.md")
    $same = $false
    try { $same = ((Resolve-Path $dest).Path -eq (Resolve-Path $skillSource).Path) } catch { $same = $false }
    if ($DryRun) {
        Say "would install skill -> $dest$(if ($exists) { ' (exists; -Force updates)' })"
        return $dest
    }
    if ($exists -and -not $Force) {
        Say "skill already present: $dest   (use -Force to refresh files)"
    } elseif (-not $same) {
        New-Item -ItemType Directory -Force $dest | Out-Null
        foreach ($item in @("SKILL.md", "README.md", "LICENSE", "scripts")) {
            $src = Join-Path $skillSource $item
            if (Test-Path $src) { Copy-Item -Recurse -Force $src $dest }
        }
        Say "installed skill: $dest"
    } else {
        Say "skill source already at target: $dest"
    }
    Write-SkillConfig (Join-Path $dest "scripts")
    return $dest
}

$agentCandidates = @(
    @{ id = "opencode";    root = (Join-Path $env:USERPROFILE ".config\opencode"); cmd = "commands";    ext = "md" },
    @{ id = "claude";      root = (Join-Path $env:USERPROFILE ".claude");           cmd = "commands";    ext = "md" },
    @{ id = "codex";       root = (Join-Path $env:USERPROFILE ".codex");            cmd = "prompts";     ext = "md" },
    @{ id = "agents";      root = (Join-Path $env:USERPROFILE ".agents");           cmd = "";            ext = "" },
    @{ id = "gemini";      root = (Join-Path $env:USERPROFILE ".gemini");           cmd = "commands";    ext = "toml" },
    @{ id = "cursor";      root = (Join-Path $env:USERPROFILE ".cursor");           cmd = "";            ext = "" },
    @{ id = "antigravity"; root = (Join-Path $env:USERPROFILE ".antigravity");      cmd = "";            ext = "" },
    @{ id = "cline";       root = (Join-Path $env:USERPROFILE ".cline");            cmd = "";            ext = "" },
    @{ id = "kimi";        root = (Join-Path $env:USERPROFILE ".kimi");             cmd = "";            ext = "" },
    @{ id = "qwen";        root = (Join-Path $env:USERPROFILE ".qwen");             cmd = "";            ext = "" },
    @{ id = "grok";        root = (Join-Path $env:USERPROFILE ".grok");             cmd = "";            ext = "" },
    @{ id = "kiro";        root = (Join-Path $env:USERPROFILE ".kiro");             cmd = "";            ext = "" },
    @{ id = "copilot";     root = (Join-Path $env:USERPROFILE ".copilot");          cmd = "";            ext = "" },
    @{ id = "continue";    root = (Join-Path $env:USERPROFILE ".continue");         cmd = "";            ext = "" }
)

$mdCommand = @'
---
description: Transcribe audio/video locally (parakeet-transcriber skill, GPU)
---
Load the `parakeet-transcriber` skill and transcribe: $ARGUMENTS
'@

$tomlCommand = @'
description = "Transcribe audio/video locally (parakeet-transcriber skill, GPU)"
prompt = "Load the parakeet-transcriber skill and transcribe: {{args}}"
'@

function Register-Command([hashtable]$agent, [string]$dest) {
    if (-not $agent.cmd) { return }
    $cmdDir = Join-Path $agent.root $agent.cmd
    if ($DryRun) { Say "would register command: $(Join-Path $cmdDir ('transcribe.' + $agent.ext))"; return }
    New-Item -ItemType Directory -Force $cmdDir | Out-Null
    if ($agent.ext -eq "toml") {
        $p = Join-Path $cmdDir "transcribe.toml"
        if (-not (Test-Path $p) -or $Force) { [System.IO.File]::WriteAllText($p, $tomlCommand, (New-Object System.Text.UTF8Encoding($false))) }
    } else {
        $p = Join-Path $cmdDir "transcribe.md"
        if (-not (Test-Path $p) -or $Force) { [System.IO.File]::WriteAllText($p, $mdCommand, (New-Object System.Text.UTF8Encoding($false))) }
    }
    Say "command registered: $p"
}

function Invoke-SmokeTest([string]$skillDir) {
    Step "Smoke test"
    $py = Get-Command python -ErrorAction SilentlyContinue
    if (-not $py) { Say "SKIP: python not found on PATH"; return $false }
    $wav = Join-Path $env:TEMP "parakeet_smoke.wav"
    if (-not $DryRun) {
        try {
            Add-Type -AssemblyName System.Speech
            $s = New-Object System.Speech.Synthesis.SpeechSynthesizer
            $voices = @($s.GetInstalledVoices() | Where-Object { $_.Enabled })
            $pt = @($voices | Where-Object { $_.VoiceInfo.Culture.Name -like "pt-*" })
            $pick = if ($pt.Count -ge 1) { $pt[0] } else { $voices[0] }
            $s.SelectVoice($pick.VoiceInfo.Name)
            $fmt = New-Object System.Speech.AudioFormat.SpeechAudioFormatInfo(
                16000, [System.Speech.AudioFormat.AudioBitsPerSample]::Sixteen,
                [System.Speech.AudioFormat.AudioChannel]::Mono)
            $s.SetOutputToWaveFile($wav, $fmt)
            $s.Speak("Bom dia! Este e um teste de instalacao do parakeet transcriber.")
            $s.SetOutputToNull(); $s.Dispose()
            Say "test audio: $wav (voice: $($pick.VoiceInfo.Name))"
        } catch {
            Say "SKIP: could not synthesize test audio ($($_.Exception.Message))"
            return $false
        }
    }
    $outDir = Join-Path $env:TEMP "parakeet_smoke_out"
    $script = Join-Path $skillDir "scripts\transcribe.py"
    Say "running: python $script <test.wav>"
    $lines = & $py.Source $script $wav --out $outDir --formats txt --force 2>&1
    $code = $LASTEXITCODE
    $txt = Get-ChildItem $outDir -Filter "*.txt" -ErrorAction SilentlyContinue | Where-Object { $_.Name -notlike "*.review.txt" } | Select-Object -First 1
    $ok = ($code -eq 0) -and $txt -and ((Get-Item $txt.FullName).Length -gt 0)
    if ($ok) {
        Say "PASS"
        foreach ($ln in $lines) { if ($ln -match "words|device|ok:") { Say "  $ln" } }
        if ($txt) { Say ("  text: " + ((Get-Content $txt.FullName -TotalCount 1) -replace "\s+", " ")) }
    } else {
        Say "FAIL (exit $code) - last output:"
        $lines | Select-Object -Last 6 | ForEach-Object { Say "  $_" }
    }
    return $ok
}

# ------------------------------------------------------------------ main

Write-Host ""
Write-Host "parakeet-transcriber installer"
Say "install:    $InstallDir"
Say "runtime:    $RuntimeDir"
Say "models:     $ModelsDir"
if ($DryRun) { Say "DRY RUN - nothing will be changed" }

if (-not $IsWindows -and $PSVersionTable.PSVersion.Major -ge 6) {
    Say "WARNING: this installer targets Windows x64; on Linux/macOS build parakeet.cpp from source."
}

$skillSource = ""
if ($PSScriptRoot -and (Test-Path (Join-Path $PSScriptRoot "SKILL.md"))) {
    $skillSource = $PSScriptRoot
    Say "skill source: local checkout ($skillSource)"
}

Step "Runtime (Vulkan, Windows x64)"
$runtimeOk = $true
if (-not $SkipRuntime) {
    $zip = Join-Path $env:TEMP "parakeet-transcriber-runtime.zip"
    $runtimeOk = Download $RunUrl $zip
    if ($runtimeOk -and -not $DryRun) {
        New-Item -ItemType Directory -Force $RuntimeDir | Out-Null
        Expand-Archive -Path $zip -DestinationPath $RuntimeDir -Force
        Say "runtime extracted: $RuntimeDir"
    }
} else {
    Say "skipped (-SkipRuntime); expecting binaries in: $RuntimeDir"
}

Step "Models (Hugging Face)"
if (-not $SkipModels) {
    if (-not (Download "$HfBase/tdt-0.6b-v3-f16.gguf" (Join-Path $ModelsDir "tdt-0.6b-v3-f16.gguf"))) { $runtimeOk = $false }
    if (-not $SkipDiarization) {
        if (-not (Download "$HfBase/nemotron-3-diarization-f16.gguf" (Join-Path $ModelsDir "nemotron-3-diarization-f16.gguf"))) { $runtimeOk = $false }
    } else {
        Say "skipped diarization model (-SkipDiarization)"
    }
} else {
    Say "skipped (-SkipModels); expecting models in: $ModelsDir"
}

Step "Skill files"
if (-not $SkipSkill) {
    if (-not $skillSource) {
        $zip = Join-Path $env:TEMP "parakeet-transcriber-main.zip"
        if (Download $ZipSrc $zip) {
            $extract = Join-Path $env:TEMP "parakeet-transcriber-src"
            if (-not $DryRun) {
                Remove-Item -Recurse -Force $extract -ErrorAction SilentlyContinue
                Expand-Archive -Path $zip -DestinationPath $extract -Force
                $skillSource = Get-ChildItem $extract -Directory | Select-Object -First 1 -ExpandProperty FullName
            } else {
                $skillSource = "<downloaded source>"
            }
        }
    }
    if ($skillSource) {
        $canonicalSkill = Install-SkillInto $SkillHome $skillSource
    }
} else {
    Say "skipped (-SkipSkill)"
}

Step "AI agents"
$installedAgents = @()
if (-not $SkipAgents -and -not $skillSource) {
    Say "skill source unavailable (do not combine -SkipSkill with agent installs); skipping agents"
} elseif (-not $SkipAgents) {
    foreach ($agent in $agentCandidates) {
        if ($Agents.Count -gt 0 -and ($Agents -notcontains $agent.id)) { continue }
        if (-not (Test-Path $agent.root)) { continue }
        $dest = Install-SkillInto (Join-Path $agent.root "skills") $skillSource
        Register-Command $agent $dest
        $installedAgents += $agent.id
    }
    if ($installedAgents.Count -eq 0) { Say "no agent directories detected" }
    else { Say ("agents: " + ($installedAgents -join ", ")) }
} else {
    Say "skipped (-SkipAgents)"
}

if (-not $DryRun) {
    [Environment]::SetEnvironmentVariable("PARAKEET_TRANSCRIBER_HOME", $InstallDir, "User")
    Say "environment: PARAKEET_TRANSCRIBER_HOME=$InstallDir (applies to new terminals/agents)"
}

$testOk = $true
if ($Test -and -not $DryRun) {
    $testOk = Invoke-SmokeTest $canonicalSkill
}

Write-Host ""
Write-Host "== Summary"
Say "runtime:  $RuntimeDir"
Say "models:   $ModelsDir"
Say "skill:    $canonicalSkill"
if ($installedAgents.Count -gt 0) { Say "agents:   $($installedAgents -join ', ')" }
if (-not $runtimeOk) { Say "WARNING: some downloads failed - re-run the installer" }
if ($Test -and -not $DryRun) { Say ("smoke test: " + $(if ($testOk) { "PASS" } else { "FAIL" })) }
Say "next: restart your agent session, then run '/transcribe <file>' or:"
Say "  python `"$canonicalSkill\scripts\transcribe.py`" <file>"
Write-Host ""
