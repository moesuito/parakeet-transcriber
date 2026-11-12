# parakeet-server-rs

A universal, always-on transcription server for this machine. The C++/Vulkan engine
(`parakeet-server`) keeps the model loaded and does the GPU work; this Rust process owns
everything around it: the HTTP API, a single-GPU queue, correction rules and a transcript cache.

- **Plain text by default.** JSON with word timestamps and SRT/VTT are opt-in.
- **No proxy, one port.** OpenAI clients (`/v1/models`, `/v1/audio/transcriptions`) and the
  richer `/transcribe` API live in the same process.
- **Diarization is optional.** Off unless a request asks for it; used for editing work.

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
| POST | `/v1/audio/transcriptions` | OpenAI-compatible; `response_format` = `text` (default), `json`, `srt`, `vtt`, `verbose_json` |
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

```bash
curl -X POST -H 'content-type: application/json' \
  -d '{"from":"reunion","to":"reunião"}' http://127.0.0.1:8800/corrections

# scope a rule to one span of the file (seconds)
curl -X POST -H 'content-type: application/json' \
  -d '{"from":"man","to":"manhã","at":[10.4,11.3]}' http://127.0.0.1:8800/corrections
```

Matching is whole-word and case-insensitive unless `"case_sensitive": true`.

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
  "retention_days": 30
}
```

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

`launch-server.vbs` runs `launch-server.cmd`, which appends stdout/stderr to
`C:\AI\stt\logs\server-rs.log`.
