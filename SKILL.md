---
name: "parakeet-transcriber"
description: "Offline, GPU-accelerated speech-to-text with word-level timestamps and optional speaker diarization (NVIDIA Parakeet TDT 0.6B v3 on Vulkan via parakeet.cpp). Produces JSON (maximum precision), SRT (subtitles) and TXT (reading). Use whenever you need to transcribe audio/video, subtitle a file, get per-word timestamps, identify who spoke, or feed an editing workflow (video-use, HyperFrames). No cloud, no API keys. Keywords: transcribe, transcription, speech-to-text, subtitles, captions, SRT, timestamps, word-level, speaker diarization, offline."
---

# parakeet-transcriber

Local **GPU-first (Vulkan)** transcription: **Parakeet TDT 0.6B v3** (25 languages,
strong Brazilian Portuguese) with **word-level timestamps**, optional **speaker
diarization** (Sortformer), three output formats, and a hash-based cache.

## When to use

- Transcribe any audio/video (mp4, mkv, mov, wav, mp3, ogg...) with **word-level timestamps**.
- Subtitles (**SRT**) or reading transcripts (**TXT**).
- Know **who said what** (up to 8 speakers) via `--diarize`.
- Feed editing workflows: the JSON is compatible with video-use's transcript expectations.

## Health check

- Runtime: `<HOME>/bin/parakeet-cli(.exe)` + `diarize(.exe)` (`<HOME>` = `~/.parakeet-transcriber`,
  or `PARAKEET_TRANSCRIBER_HOME`, or paths in a local `config.json` next to the script).
- Models: `<HOME>/models/tdt-0.6b-v3-f16.gguf` (required),
  `<HOME>/models/nemotron-3-diarization-f16.gguf` (optional, for `--diarize`).
- If something is missing, read the repository **README.md** — it contains the full setup.
- A healthy run logs a `[parakeet] using device: Vulkan0`-style line. CPU fallback:
  `PARAKEET_DEVICE=Vulkan0` (or force `cpu` only for debugging).

## Usage

```bash
python <skill>/scripts/transcribe.py <media> [options]
```

| Option | Default | Description |
|---|---|---|
| `--out DIR` | `<media_parent>/transcripts` | output directory |
| `--quant f16\|q8_0` | f16 | model precision (f16 = maximum accuracy) |
| `--diarize auto\|on\|off` | auto | speaker attribution (on when the model is installed) |
| `--audio-track N` | auto (most channels) | which audio stream to use |
| `--formats json,srt,txt[,vtt]` | json,srt,txt | artifacts to write |
| `--lang pt` | — | forwarded to the runtime; ignored by the v3 model (detection is built-in) |
| `--force` | — | ignore the cache |

Examples:

```bash
# interview with 2+ speakers -> all three formats
python <skill>/scripts/transcribe.py "C:\videos\interview.mp4"

# subtitles only, fastest quant
python <skill>/scripts/transcribe.py "C:\videos\podcast.mkv" --quant q8_0 --formats srt

# drop into a video-use project (JSON only)
python <skill>/scripts/transcribe.py "C:\videos\C0103.MP4" --out "C:\videos\edit\transcripts" --formats json
```

## Outputs

| File | Focus | Content |
|---|---|---|
| `<stem>.json` | **maximum precision** | `text` + `words[]` with `{text,start,end,type,confidence,speaker_id?}` (word + spacing entries) |
| `<stem>.srt` | subtitles | cues grouped by speech gaps and punctuation |
| `<stem>.txt` | reading | context blocks, one `[MM:SS]`/`[HH:MM:SS]` stamp per block, speaker prefix when diarized |
| `.meta/<stem>.meta.json` | bookkeeping | engine, hashes, RTF — also the cache key (keep, don't parse as transcript) |

Never re-transcribe an unchanged file: the cache (source hash + settings) handles it.

## Review & repair (agent workflow)

Every run also writes `<stem>.review.txt` (words below the confidence threshold, plus this
guidance). Review is **contextual**, not only about language drift — deliver clean SRT/TXT:

1. Read the full text and the review report. Flag what the context makes suspicious:
   low-confidence/garbled words, language-drift artifacts ("reunião" → "reunion" after an
   English passage), wrong homophones, names and technical terms, and words consistently
   misheard across the file. Confidence alone does not catch everything — judge the text.
2. Repair with tooling that keeps subtitles in sync. **SRT/TXT/VTT are the corrected
   deliverables; the JSON keeps the raw capture and its timings.**
   - Word fixes (homophones, names, consistent mishears): write a `fixes.json` and run
     `python transcribe.py <media> --apply-corrections fixes.json`
     (`{"replacements": [{"from": "reunion", "to": "reunião"}, {"from": "man", "to": "manhã", "at": [10.4, 11.3]}]}`
     — whole-word, case-insensitive; `at` optional to scope by time; no `at` = every occurrence).
     Corrections are saved as `<stem>.corrections.json` and re-applied on future renders.
   - Garbled spans (e.g. drift): `python transcribe.py <media> --retranscribe "START-END"` for a
     fresh-context transcription of just that span, then fix words with the rule above.
3. Never hand-edit SRT timings, and never rewrite the JSON words — corrections go through
   `--apply-corrections`.
4. `--tokens` adds token-level timestamps and confidences (token text resolved from the model
   vocab) to the JSON for maximum-precision inspection.

## Golden rules

1. GPU only for real work (Vulkan); CPU is just a diagnostic path.
2. Timestamps are word-level and stable. When cutting video, never cut inside a word —
   snap to JSON boundaries and keep 30–200 ms padding.
3. Speaker ids are `speaker_0`, `speaker_1`, ... by order of first appearance; don't invent names.
4. Numbers come out as digits (`342`, `9h30`) — that is expected, don't "fix" it in the JSON.
5. Large batches: prefer a server-based flow (keep the model loaded) to avoid heavy
   per-file GPU churn on Windows.
