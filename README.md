# parakeet-transcriber

**Offline, GPU-accelerated speech-to-text for AI agents and humans.**
Runs NVIDIA **[Parakeet TDT 0.6B v3](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3)**
(25 languages, strong Brazilian Portuguese) on **Vulkan** via
[parakeet.cpp](https://github.com/mudler/parakeet.cpp), with **word-level timestamps**,
optional **speaker diarization** (Sortformer), and three output formats:

| Format | Focus | Content |
| --- | --- | --- |
| `json` | **maximum precision** | word-level timestamps + per-word confidence (+ speaker ids) |
| `srt` | **subtitles** | cues grouped by speech gaps and punctuation, ready to burn |
| `txt` | **reading** | context blocks with a single timestamp per block |

No cloud, no API keys, no account. One model file, one binary folder, done.

---

## Quick install (one line)

```powershell
irm https://raw.githubusercontent.com/moesuito/parakeet-transcriber/main/install.ps1 | iex
```

Works on **Windows PowerShell 5.1 and PowerShell 7+**. The installer:

1. downloads the **Vulkan runtime** from this repo's Releases;
2. downloads the **model** (Parakeet TDT 0.6B v3 f16, 1.37 GB) and the optional
   **diarization model** (191 MB) from Hugging Face;
3. installs the skill into **every AI-agent directory it detects** — OpenCode, Claude Code,
   Codex, Gemini, Cursor, Antigravity, Cline, Kimi, Qwen, Grok, Kiro, Copilot, Continue and
   the generic `~/.agents/skills` standard — plus a canonical copy at `<INSTALL>\skill`;
4. registers a `/transcribe` command where the agent supports it (OpenCode, Claude Code,
   Codex, Gemini);
5. writes a per-install `config.json`, sets `PARAKEET_TRANSCRIBER_HOME`, and optionally runs
   a **smoke test** (TTS sample → real GPU transcription) with `-Test`.

Default install dir: `%USERPROFILE%\.parakeet-transcriber`. **Restart your agent** after install.

Prefer control? Download `install.ps1` and run it with flags (both PS editions):

```powershell
.\install.ps1 -SkipDiarization -Test
.\install.ps1 -InstallDir D:\AI\parakeet -DryRun        # preview, changes nothing
.\install.ps1 -RuntimeDir C:\AI\stt\bin\master -ModelsDir C:\AI\stt\models -SkipRuntime -SkipModels
```

| Flag | Meaning |
| --- | --- |
| `-InstallDir DIR` | install root (default `%USERPROFILE%\.parakeet-transcriber`) |
| `-RuntimeDir DIR` / `-ModelsDir DIR` | use existing binaries/models (default `<INSTALL>\bin` / `<INSTALL>\models`) |
| `-SkipRuntime` / `-SkipModels` | keep what already exists |
| `-SkipDiarization` | skip the optional 191 MB diarization model |
| `-SkipAgents` / `-SkipSkill` | do not touch agent dirs / do not (re)install the skill copy |
| `-Agents id1,id2` | restrict agent registration to these ids |
| `-Test` | run the smoke test at the end |
| `-Force` | re-download / overwrite existing files |
| `-DryRun` | print the plan, change nothing |

> **GPU:** the runtime uses **Vulkan** — any Vulkan-capable GPU works (AMD, NVIDIA, Intel; the
> loader ships with the GPU driver). Without Vulkan it falls back to CPU (much slower); force
> a device with `PARAKEET_DEVICE` (`Vulkan0`, `Vulkan1`, `cpu`).

### Agent prompt (copy this)

> Install the parakeet-transcriber skill on this machine:
> `irm https://raw.githubusercontent.com/moesuito/parakeet-transcriber/main/install.ps1 | iex`
> Then follow the repository README "Verification checklist (agents)" and report what was
> installed (paths, detected agents, smoke test result).

## Manual install (fallback)

*(the one-liner above automates everything below)*

**You are (probably) an AI agent installing this skill for your user.**
Follow these steps in order, then verify and report what you installed.

### 0. Prerequisites

- **Windows x64** with a Vulkan-capable GPU → use the prebuilt binaries from this
  repository's [Releases](https://github.com/moesuito/parakeet-transcriber/releases/latest).
  *(Linux / macOS: build or download `parakeet-cli` from
  [mudler/parakeet.cpp releases](https://github.com/mudler/parakeet.cpp/releases) —
  diarization additionally needs a build that ships the `diarize` example.)*
- `python` 3.10+ and `ffmpeg`/`ffprobe` available on PATH.
- Free disk: ~2.5 GB (binaries + f16 model + optional 191 MB diarization model).

### 1. Choose the home directory

Default: `~/.parakeet-transcriber` (Windows: `%USERPROFILE%\.parakeet-transcriber`).
To use another folder, set the environment variable `PARAKEET_TRANSCRIBER_HOME`.

Expected layout:

```
<HOME>/
├── bin/      parakeet-cli(.exe), diarize(.exe), ggml-*.dll
└── models/   tdt-0.6b-v3-f16.gguf, nemotron-3-diarization-f16.gguf
```

### 2. Download the runtime (binaries)

- **Windows x64 (recommended):** download `parakeet-transcriber-win-x64-vulkan.zip`
  from [Releases](https://github.com/moesuito/parakeet-transcriber/releases/latest)
  and extract **everything into `<HOME>/bin/`**.
  (Built from `mudler/parakeet.cpp` master; includes `parakeet-cli`, `diarize` and the
  Vulkan backend. The Vulkan *loader* ships with your GPU driver — nothing else to install.)
- **Other platforms:** see [mudler/parakeet.cpp releases](https://github.com/mudler/parakeet.cpp/releases)
  (Linux x64 CPU/Vulkan/CUDA, macOS arm64 Metal). Note that older releases may not include
  the `diarize` binary.

### 3. Download the models (Hugging Face)

| File | Size | Required | URL |
| --- | --- | --- | --- |
| `tdt-0.6b-v3-f16.gguf` | 1.37 GB | **Yes** | https://huggingface.co/mudler/parakeet-cpp-gguf/resolve/main/tdt-0.6b-v3-f16.gguf |
| `nemotron-3-diarization-f16.gguf` | 191 MB | Optional (speakers) | https://huggingface.co/mudler/parakeet-cpp-gguf/resolve/main/nemotron-3-diarization-f16.gguf |
| `tdt-0.6b-v3-q8_0.gguf` | 897 MB | Optional (smaller) | https://huggingface.co/mudler/parakeet-cpp-gguf/resolve/main/tdt-0.6b-v3-q8_0.gguf |

Place them in **`<HOME>/models/`**.

### 4. Register the skill

Copy (or symlink) this repository into your agent's **skills directory**, keeping the
folder name `parakeet-transcriber` (it is the skill id):

- **OpenCode** → `~/.config/opencode/skills/parakeet-transcriber/`
- **Claude Code** → `~/.claude/skills/parakeet-transcriber/`
- **Codex / others** → any `skills/` directory your agent scans; what matters is that
  `SKILL.md` sits at the folder root.

### 5. Verify

```bash
python <skill>/scripts/transcribe.py --help
python <skill>/scripts/transcribe.py <some-audio-or-video-file> --formats json,srt,txt
```

A successful run prints `ok: N words ->` followed by the artifact paths. If the runtime or
model is not found, either download it (steps 2–3) or create a `config.json` next to
`scripts/transcribe.py` pointing at your paths (see *Configuration* below).

### 6. Optional — register a slash command (OpenCode)

Create `~/.config/opencode/commands/transcribe.md`:

```markdown
---
description: Transcribe audio/video locally (Parakeet v3 + diarization, GPU)
---
Load the `parakeet-transcriber` skill and transcribe: $ARGUMENTS
```

---

## Verification checklist (agents)

After running the installer, verify and report to the user:

1. **Files** — the canonical install exists:
   `<INSTALL>\skill\parakeet-transcriber\scripts\transcribe.py`, and every detected agent has
   `<agent-root>\skills\parakeet-transcriber\SKILL.md`.
2. **CLI** — `python <skill>\scripts\transcribe.py --help` prints the usage (Python 3.10+).
3. **Real run** — transcribe anything
   (`python <skill>\scripts\transcribe.py <file> --formats txt --quiet`) and expect
   `ok: N words ->` plus a `using device: Vulkan0`-style line; or re-run the installer with
   `-SkipRuntime -SkipModels -Test` for the built-in smoke test.
4. **Report** — install dir, detected agents, smoke-test result, and remind the user to
   restart the agent session so `/transcribe` (where registered) becomes available.

## Usage

```bash
# interview with multiple speakers — all three formats
python scripts/transcribe.py "C:\videos\interview.mp4"

# subtitles only, smaller/faster quant
python scripts/transcribe.py "C:\videos\podcast.mkv" --quant q8_0 --formats srt

# feed a video-use project (JSON only, written where the editor reads it)
python scripts/transcribe.py "C:\videos\C0103.MP4" --out "C:\videos\edit\transcripts" --formats json
```

| Option | Default | Description |
| --- | --- | --- |
| `--out DIR` | `<media_parent>/transcripts` | output directory |
| `--quant f16\|q8_0` | `f16` | model precision (f16 = maximum accuracy; q8_0 ≈ same quality, 35% smaller) |
| `--diarize auto\|on\|off` | `auto` | speaker attribution (on when the diarization model is installed) |
| `--audio-track N` | auto (most channels) | which audio stream to use (OBS: 0 = game, 1 = mic) |
| `--formats json,srt,txt[,vtt]` | `json,srt,txt` | artifacts to write |
| `--lang pt` | — | forwarded to the runtime; **ignored by the v3 model** (language detection is built-in) |
| `--tokens` | off | JSON also carries token-level timestamps/confidences (token text via the model vocab) |
| `--review-threshold F` | `0.6` | confidence threshold for the review report |
| `--retranscribe "START-END"` | — | fresh-context re-transcription of one span (agent repair) |
| `--apply-corrections FILE` | — | apply a word-fixes JSON to SRT/TXT/VTT (raw JSON stays untouched; saved for future renders) |
| `--keep-raw-json` | off | leave `<stem>.json` untouched; by default corrections are carried into **all deliverables** (json + srt + txt; timings untouched, original text kept as `raw_text`) |
| `--force` | — | ignore the cache |

## Configuration

Path resolution order:

1. explicit arguments (`--cli`, `--model`, `--diar-model`)
2. `config.json` next to `scripts/transcribe.py` (optional, git-ignored)
3. `PARAKEET_TRANSCRIBER_HOME` environment variable
4. default `~/.parakeet-transcriber`

`config.json` example (only needed for custom layouts):

```json
{
  "cli": "C:\\AI\\stt\\bin\\master\\parakeet-cli.exe",
  "diarize_exe": "C:\\AI\\stt\\bin\\master\\diarize.exe",
  "models": {
    "f16": "C:\\AI\\stt\\models\\tdt-0.6b-v3-f16.gguf",
    "q8_0": "C:\\AI\\stt\\models\\tdt-0.6b-v3-q8_0.gguf",
    "diarization": "C:\\AI\\stt\\models\\nemotron-3-diarization-f16.gguf"
  },
  "defaults": { "quant": "f16", "diarize": true, "formats": ["json", "srt", "txt"] }
}
```

## Outputs

### JSON — maximum precision

```json
{
  "text": "Hello world. How are you?",
  "words": [
    { "text": "Hello", "start": 0.32, "end": 0.64, "type": "word", "confidence": 0.99 },
    { "text": "",      "start": 0.64, "end": 0.72, "type": "spacing" },
    { "text": "How",   "start": 0.72, "end": 0.85, "type": "word", "speaker_id": "speaker_0" }
  ],
  "meta": { "source": "...", "duration_s": 12.3, "model": "tdt-0.6b-v3-f16.gguf", "quant": "f16", "diarized": true }
}
```

- `words[]` mixes `type: "word"` entries and `type: "spacing"` gap markers — the spacing
  entries preserve silence information (used by word-precise editing flows).
- `speaker_id` appears when diarization ran; `confidence` is the per-word model confidence.
- Times are seconds (float, 3 decimals).

### SRT — subtitles

Standard SubRip, one cue per short group of words (breaks on ≥ 0.8 s gaps, punctuation,
and length limits). Drop straight into any player/editor or caption burn.

### TXT — reading

Paragraph-like blocks separated by pauses (≥ 2 s, or ≥ 1 s after sentence punctuation),
each prefixed with one timestamp — for reading, notes and LLM consumption:

```
[00:04] Hello world. How are you doing today? I hope everything is fine on your side.

[00:21] So, about the project — we reviewed the schedule and the budget last week.
```

When diarization is on, blocks are prefixed with `Speaker N:`.

### Review report & repair (for agents)

Every run writes `<stem>.review.txt` (words below the confidence threshold + guidance).
Combined with a read-through of the text, this powers an agentic review loop — **contextual,
not only language drift**:

```bash
# 1. transcribe (raw JSON + review report)
python scripts/transcribe.py interview.mp4 --tokens

# 2. read review.txt and the text: fix context errors (homophones, names, technical
#    terms, consistent mishears); for a garbled span, re-transcribe just it (fresh context)
python scripts/transcribe.py interview.mp4 --retranscribe "68.2-74.5"

# 3. apply word fixes (whole-word, case-insensitive; optional "at" time scoping)
python scripts/transcribe.py interview.mp4 --apply-corrections fixes.json
```

`fixes.json`:

```json
{ "replacements": [
    { "from": "reunion", "to": "reunião" },
    { "from": "man",     "to": "manhã", "at": [10.4, 11.3] }
  ] }
```

Corrections are applied to **all three deliverables** (json + srt + txt). Timings are never
changed, and the original text of each corrected word is preserved as `raw_text` inside the
JSON (`--keep-raw-json` opts out and leaves `<stem>.json` with the raw capture). Corrections
are saved as `<stem>.corrections.json` next to the outputs and **re-applied automatically on
future renders** — even a full `--force` re-run regenerates the JSON already corrected.
Never hand-edit SRT timings.

`--retranscribe` writes `<stem>.rt_START-END.json/.txt` with **absolute** times.
`--tokens` adds token-level timestamps/confidences (token text resolved from the model vocab).

## Language handling

Parakeet TDT 0.6B v3 detects the language automatically (25 European languages) with no
configuration or prompting — the `--lang` flag is forwarded to the runtime but has **no
effect** on this model.

What works well:

- Single-language audio, any of the 25 languages (automatic detection).
- Short borrowings inside a sentence — English tech terms inside Portuguese speech
  ("Precisamos atualizar o deployment pipeline antes da reunião") transcribe correctly.

Known limitation — language drift on long stretches:

With no language conditioning, a long passage in one language can bias the decoder for what
comes next: after an English passage, following Portuguese may be rendered through English
near-homophones (observed: "reunião" → "reunion", "manhã" → "man"). Conversely, Portuguese
words spoken with an American accent can turn into English words ("fazer o" → "phaser all").
If your material is heavily bilingual, transcribe each language stretch as a **separate
file/run** (fresh decoder context per language) for best results.

Measured examples (synthetic TTS test set):

| Scenario | Result |
| --- | --- |
| Portuguese sentence → English sentences → Portuguese sentence | English perfect; the trailing Portuguese degrades |
| Portuguese sentence with an English noun phrase in the middle | **Perfect** |
| Brazilian voice reading a long English fragment (BR accent) | Garbled ("portinglês") |
| American voice reading Portuguese words | Garbled |

## Performance

Measured on **AMD Radeon RX 9060 XT 16 GB** (Vulkan, f16, median over 386 test clips):
**≈ 10–15× realtime per file with the CLI** (model load included — roughly 1 hour of audio
in ~5 minutes). Keeping the model resident via `parakeet-server` (shipped by parakeet.cpp)
reaches ~100× realtime for bulk runs. VRAM: ~2 GB ASR, ~4 GB with diarization.
Quality reference (FLEURS pt-BR dev, 386 clips): **4.8% WER**, median 0% (more than half of
the clips come out perfect).

## Licenses and credits

- This project (skill, `scripts/transcribe.py`, `install.ps1`, docs): **Apache-2.0** © 2026 João Alano
  — see [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE).
- **NVIDIA** — the models this project runs:
  - [Parakeet TDT 0.6B v3](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3) speech recognition
    weights: **CC-BY-4.0** © NVIDIA.
  - [Nemotron-3-Diarization](https://huggingface.co/nvidia/Nemotron-3-Diarization) (Sortformer)
    speaker-diarization weights: **OpenMDW-1.1** © NVIDIA.
- **[parakeet.cpp](https://github.com/mudler/parakeet.cpp)** — the **MIT**-licensed C++/ggml
  inference runtime by the **LocalAI team** (Ettore Di Giacinto, Richard Palethorpe). The
  prebuilt Windows binaries in this repository's Releases are compiled from it (source commit
  recorded in the release notes).
- **[ggml](https://github.com/ggml-org/ggml)** — **MIT**-licensed tensor library used by the runtime.
- **[mudler/parakeet-cpp-gguf](https://huggingface.co/mudler/parakeet-cpp-gguf)** — GGUF conversions
  of the NVIDIA weights used by the installer.
- Model weights are **downloaded by the installer from Hugging Face and are not redistributed**
  in this repository; each weight keeps the license of its source (links above).

## Troubleshooting

- **`parakeet-cli not found`** → check `<HOME>/bin` (or `config.json`) and that
  `PARAKEET_TRANSCRIBER_HOME` points at the right folder.
- **Runs on CPU / slow** → make sure the Vulkan driver is installed; the log should show
  `using device: Vulkan0`. Override with `PARAKEET_DEVICE=Vulkan0` (or `Vulkan1`).
- **`track is silent`** → the chosen audio stream has no signal; try `--audio-track N`.
- **Windows Defender / AV slows large batches** → exclude the models folder from real-time
  scanning (the model is read for every CLI invocation).
- **Diarization not running** → it needs the `diarize` binary in `<HOME>/bin` and
  `nemotron-3-diarization-f16.gguf` in `<HOME>/models`.
