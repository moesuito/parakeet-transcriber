# parakeet-server-rs

A universal, always-on transcription server for this machine. The C++/Vulkan engine
(`parakeet-server`) keeps the model loaded and does the GPU work; this Rust process owns
everything around it: the HTTP API, a single-GPU queue, correction rules and a transcript cache.

- **Plain text by default.** JSON with word timestamps and SRT/VTT are opt-in.
- **No proxy, one port.** OpenAI clients (`/v1/models`, `/v1/audio/transcriptions`) and the
  richer `/transcribe` API live in the same process.
- **Diarization is optional.** Off unless a request asks for it; used for editing work.

## How it runs

It is a **headless server**: no window, no tray icon, no user interaction. It runs on this
machine **24 hours a day** and every tool that needs speech-to-text points at the same local
address (`http://127.0.0.1:8800`).

```
logon
 └─ Task Scheduler: "parakeet-server"  (wscript //B → launch-server.cmd)
     └─ parakeet-server-rs.exe        :8800   HTTP, queue, corrections, cache
         └─ parakeet-server.exe       :8799   C++/Vulkan engine, one model in VRAM
```

- Started at **logon** by the scheduled task, which restarts it a minute after any exit
  (up to 999 times).
- The engine is a child of the server: started when missing, restarted when it stops
  answering (checked every 20 s), and killed with the server.
- Model loading is lazy and the engine unloads after **240 s idle**; a request after that pays
  ~1.5 s for the reload. The server itself stays up the whole time.
- Logs are the server's own (`C:\AI\stt\logs\server.log.YYYY-MM-DD`), not a shell redirect,
  so the engine can never lock the file.

## Why Rust here

The ASR itself stays in C++/Vulkan (parakeet.cpp) because that is where the GPU kernels live.
Everything that is I/O and bookkeeping — HTTP, multipart uploads, ffmpeg calls, queueing,
corrections, caching, SQLite — is Rust: one small static binary, no runtime, starts in
milliseconds, and stays up for weeks.

## Endpoints

| Method | Path | What it does |
|---|---|---|
| GET | `/health` | engine reachable, queue depth, model name |
| GET | `/v1/models` | OpenAI model list (one entry) |
| POST | `/v1/audio/transcriptions` | OpenAI-compatible; `response_format` = `text` (default), `json`, `srt`, `vtt`, `verbose_json`. Send `diarize=on` as a form field for speakers. |
| POST | `/transcribe` | rich result: `?format=text\|json\|srt\|vtt\|all`, `?diarize=on` |
| GET | `/jobs` | recent jobs with status, duration and text |
| GET | `/jobs/{id}` | one job |
| DELETE | `/jobs/{id}` | forget a job |
| GET/POST | `/corrections` | list or add a word correction |
| DELETE | `/corrections/{id}` | remove a correction |
| POST | `/corrections/import` | import the skill's `fixes.json` (`{"replacements":[…]}`) |

Any input format ffmpeg can decode (mp4, mkv, mp3, m4a, ogg, wav…) is accepted; the server
converts to 16 kHz mono WAV for the engine.

### Examples

```bash
# plain text (default)
curl -F file=@talk.mp3 http://127.0.0.1:8800/v1/audio/transcriptions

# word timestamps
curl -F file=@talk.mp3 -F response_format=verbose_json -F 'timestamp_granularities[]=word' \
  http://127.0.0.1:8800/v1/audio/transcriptions

# subtitles
curl -F file=@talk.mp3 -F response_format=srt http://127.0.0.1:8800/v1/audio/transcriptions

# speaker labels (editing work only)
curl -F file=@interview.mp4 "http://127.0.0.1:8800/transcribe?format=json&diarize=on"
```

When diarization runs, `verbose_json` carries the speakers everywhere:

```json
{
  "speakers": ["speaker_0", "speaker_1"],
  "words":    [{ "word": "Mr", "start": 0.24, "end": 0.72, "speaker": "speaker_0" }],
  "segments": [{ "id": 0, "start": 0.24, "end": 5.04, "text": "Mr Quilter is …", "speaker": "speaker_0" }]
}
```

`response_format=srt` and `vtt` prefix each cue with `Speaker N:` when a cue has one voice.
`diarize` accepts `on`/`true`/`1`; the field can be named `diarize`, `diarization` or `speakers`.
Without it, nothing changes: plain transcription never runs the speaker pass.

OpenAI Python client:

```python
from openai import OpenAI
client = OpenAI(base_url="http://127.0.0.1:8800/v1", api_key="not-needed")
with open("talk.mp3", "rb") as f:
    print(client.audio.transcriptions.create(model="parakeet-ultra", file=f).text)
```

## Corrections

Rules are stored in SQLite and applied to **every** transcript (text, JSON, SRT, VTT) before
it is returned. The original word is kept in `raw_text`. The cache stores the raw result, so a
new rule applies immediately to files already transcribed — no re-run needed.

Matching is word-boundary and case-insensitive by default, follows the
[`parakeet-corrections`](https://github.com/Caffa/parakeet-corrections) conventions, and
supports short phrases: `"open code"` becomes one token `"OpenCode"` and keeps the first
word's timing. The correction inherits the input's case (`OLAMA` → `OLLAMA`).

```bash
curl -X POST -H 'content-type: application/json' \
  -d '{"from":"reunion","to":"reunião"}' http://127.0.0.1:8800/corrections

# scope a rule to one span of the file (seconds)
curl -X POST -H 'content-type: application/json' \
  -d '{"from":"man","to":"manhã","at":[10.4,11.3]}' http://127.0.0.1:8800/corrections
```

### Community lexicons

`corrections_file` points at a `parakeet-corrections` JSON (`data/all.json`). It is imported on
start, and again whenever the file's hash changes; a copy ships in
[`data/community-corrections.json`](data/community-corrections.json). Imported rules carry
`"source": "community"` and are replaced as a block, so hand-made rules are never touched.

```bash
# after pulling a newer lexicon
curl -X POST http://127.0.0.1:8800/corrections/reload
```

| Source | Behaviour |
|---|---|
| `community` | replaced on every import; comes from the lexicon file |
| `user` | added through the API; survives imports |

## Disk usage

| Data | Where | Kept |
|---|---|---|
| Transcripts (text + words + SRT/VTT) | `data/server.db` (`jobs`, `cache`) | forever (`retention_days: 0`) |
| Original audio | `audio_dir` | newest `audio_keep` files (default **20**), FIFO |
| Working WAV | system temp | deleted at the end of each job |

The audio archive is a ring buffer: after each job the server lists the directory (names start
with a UTC timestamp, so they sort chronologically) and deletes the oldest file whenever the
count goes past `audio_keep`. It is trimmed on start too. `audio_keep: 0` keeps no audio at
all; transcripts are unaffected either way. Each job reports its file as `audio`, e.g.:

```json
{ "id": "94d12bdc-...", "source": "audio.wav", "audio": "C:\\AI\\stt\\data\\audio\\20261112T213715Z-94d12bdc.wav" }
```

## Configuration

`C:\AI\stt\config\server.json` (written with defaults on first run):

```json
{
  "bind": "127.0.0.1:8800",
  "model_id": "parakeet-ultra",
  "engine": {
    "exe": "C:\\AI\\STT\\bin\\master\\parakeet-server.exe",
    "model": "C:\\Users\\Alano\\.parakeet-transcriber\\models\\parakeet-ultra.f16.gguf",
    "port": 8799,
    "args": ["--keep-loaded", "--idle-timeout", "0"],
    "startup_timeout_secs": 180,
    "external": false
  },
  "database": "C:\\AI\\stt\\data\\server.db",
  "queue": { "capacity": 64, "job_timeout_secs": 900 },
  "max_upload_mb": 512,
  "default_response_format": "text",
  "cli": "C:\\AI\\STT\\bin\\master\\parakeet-cli.exe",
  "diar_model": "C:\\AI\\STT\\models\\nemotron-3-diarization-f16.gguf",
  "retention_days": 0,
  "audio_keep": 20,
  "audio_dir": "C:\\AI\\stt\\data\\audio",
  "corrections_file": "C:\\AI\\stt\\config\\community-corrections.json",
  "log_dir": "C:\\AI\\stt\\logs"
}
```

`retention_days` (0 = keep forever) only trims finished jobs; transcripts are the point of the
server, so the default keeps them. `audio_keep` caps the audio archive, not the transcripts.

`PARAKEET_BIND`, `PARAKEET_MODEL_ID` and `PARAKEET_LOG` override parts of it at start.

The server starts the engine itself when it is not answering and restarts it if it dies
(`external: true` disables that).

## Build

```bash
cd server
cargo build --release      # target/release/parakeet-server-rs.exe
cargo test                 # formats + correction rules
```

## Run 24/7 (Windows)

```powershell
# a task at logon, restarted a minute after any exit
$action   = New-ScheduledTaskAction -Execute "wscript.exe" -Argument '//B "C:\AI\stt\bin\launch-server.vbs"'
$trigger  = New-ScheduledTaskTrigger -AtLogOn -User "$env:USERNAME"
$settings = New-ScheduledTaskSettingsSet -RestartCount 999 -RestartInterval (New-TimeSpan -Minutes 1)
Register-ScheduledTask -TaskName "parakeet-server" -Action $action -Trigger $trigger -Settings $settings -Force
```

`launch-server.vbs` runs `launch-server.cmd`, which starts the binary. The server writes its own
log, so the launcher needs no redirection.

## Credits

The bundled `data/community-corrections.json` comes from
[Caffa/parakeet-corrections](https://github.com/Caffa/parakeet-corrections) (MIT, © 2026 Caffa).
