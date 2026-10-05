#!/usr/bin/env python3
"""transcribe.py - local speech-to-text with NVIDIA Parakeet TDT 0.6B v3.

Runs parakeet.cpp (ggml/Vulkan) on the GPU and writes three artifacts per media
file (plus metadata):

    <stem>.json   word-level timestamps + confidences (maximum precision)
    <stem>.srt    subtitles (cue-grouped words)
    <stem>.txt    reading-focused transcript (few timestamps, context blocks)
    <stem>.vtt    WebVTT (optional, via --formats)

Optionally adds speaker diarization (NVIDIA Nemotron-3-Diarization / Sortformer,
via the `diarize` binary shipped with parakeet.cpp), attributing every word to a
speaker ("speaker_N" in JSON, "Speaker N" in TXT).

Path resolution (runtime + models):
    1) explicit arguments (--cli / --model / --diar-model)
    2) config.json next to this script (optional; git-ignored in the repo)
    3) PARAKEET_TRANSCRIBER_HOME environment variable
    4) default home: ~/.parakeet-transcriber   (expects bin/ and models/ inside)

Example config.json:
    {
      "home": "C:\\\\AI\\\\stt",
      "cli": "C:\\\\AI\\\\stt\\\\bin\\\\master\\\\parakeet-cli.exe",
      "diarize_exe": "C:\\\\AI\\\\stt\\\\bin\\\\master\\\\diarize.exe",
      "models": {
        "f16": "C:\\\\AI\\\\stt\\\\models\\\\tdt-0.6b-v3-f16.gguf",
        "q8_0": "C:\\\\AI\\\\stt\\\\models\\\\tdt-0.6b-v3-q8_0.gguf",
        "diarization": "C:\\\\AI\\\\stt\\\\models\\\\nemotron-3-diarization-f16.gguf"
      },
      "defaults": {"quant": "f16", "diarize": true, "formats": ["json", "srt", "txt"]}
    }

Usage:
    python transcribe.py <media> [--out DIR] [--quant f16|q8_0] [--lang pt]
        [--diarize auto|on|off] [--audio-track N] [--formats json,srt,txt]
        [--force] [--quiet]
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import subprocess
import sys
import tempfile
import time
import wave
from datetime import datetime, timezone
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
CONFIG_PATH = SCRIPT_DIR / "config.json"

if hasattr(sys.stdout, "reconfigure"):
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
        sys.stderr.reconfigure(encoding="utf-8", errors="replace")
    except Exception:
        pass


def log(msg: str) -> None:
    print(msg, flush=True)


# ---------------------------------------------------------------- paths

def load_config() -> dict:
    if CONFIG_PATH.exists():
        try:
            return json.loads(CONFIG_PATH.read_text(encoding="utf-8"))
        except Exception as e:
            raise SystemExit(f"config.json is invalid: {e}")
    return {}


def resolve_home(cfg: dict) -> Path:
    env = os.environ.get("PARAKEET_TRANSCRIBER_HOME")
    if env:
        return Path(env).expanduser()
    if cfg.get("home"):
        return Path(cfg["home"]).expanduser()
    return Path.home() / ".parakeet-transcriber"


def _first_existing(candidates: list[Path]) -> Path | None:
    for p in candidates:
        if p.exists():
            return p
    return None


def resolve_cli(cfg: dict, home: Path, override: str | None) -> Path:
    if override:
        p = Path(override)
        if p.exists():
            return p
        raise SystemExit(f"--cli not found: {p}")
    p = _first_existing([Path(cfg["cli"])] if cfg.get("cli") else [])
    if p:
        return p
    p = _first_existing([home / "bin" / n for n in ("parakeet-cli.exe", "parakeet-cli")])
    if p:
        return p
    raise SystemExit(
        "parakeet-cli not found.\n"
        f"  Looked in: {home / 'bin'}\n"
        "  Fix: download the runtime into <HOME>/bin, set PARAKEET_TRANSCRIBER_HOME,\n"
        "  or create a config.json next to this script (see README)."
    )


def resolve_diarize(cfg: dict, home: Path, cli: Path, override: str | None) -> Path | None:
    if override:
        p = Path(override)
        return p if p.exists() else None
    p = _first_existing([Path(cfg["diarize_exe"])] if cfg.get("diarize_exe") else [])
    if p:
        return p
    return _first_existing([
        cli.parent / "diarize.exe",
        cli.parent / "diarize",
        home / "bin" / "diarize.exe",
        home / "bin" / "diarize",
    ])


def resolve_model(cfg: dict, home: Path, quant: str, override: str | None) -> Path:
    if override:
        p = Path(override)
        if p.exists():
            return p
        raise SystemExit(f"--model not found: {p}")
    models = cfg.get("models") or {}
    if models.get(quant):
        return Path(models[quant])
    names = {"f16": "tdt-0.6b-v3-f16.gguf", "q8_0": "tdt-0.6b-v3-q8_0.gguf"}
    p = home / "models" / names[quant]
    if not p.exists():
        raise SystemExit(
            f"model not found: {p}\n"
            "  Download it from https://huggingface.co/mudler/parakeet-cpp-gguf (see README)."
        )
    return p


def resolve_diar_model(cfg: dict, home: Path, override: str | None) -> Path | None:
    if override:
        p = Path(override)
        return p if p.exists() else None
    models = cfg.get("models") or {}
    if models.get("diarization"):
        p = Path(models["diarization"])
        return p if p.exists() else None
    p = home / "models" / "nemotron-3-diarization-f16.gguf"
    return p if p.exists() else None


# ---------------------------------------------------------------- helpers

def sha256_file(path: Path, chunk: int = 1 << 20) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while True:
            b = f.read(chunk)
            if not b:
                break
            h.update(b)
    return h.hexdigest()


def run(cmd: list[str], **kw) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8",
                          errors="replace", **kw)


def ffprobe_streams(src: Path) -> tuple[list[dict], float]:
    p = run(["ffprobe", "-v", "error", "-print_format", "json",
             "-show_streams", "-show_format", str(src)])
    if p.returncode != 0:
        raise SystemExit(f"ffprobe failed: {p.stderr.strip()[:400]}")
    data = json.loads(p.stdout)
    audio = [s for s in data.get("streams", []) if s.get("codec_type") == "audio"]
    dur = float(data.get("format", {}).get("duration") or 0.0)
    return audio, dur


def pick_track(audio: list[dict], audio_track: int | None) -> int:
    if not audio:
        raise SystemExit("the file has no audio stream")
    if audio_track is not None:
        if audio_track >= len(audio):
            raise SystemExit(f"audio track {audio_track} does not exist ({len(audio)} tracks)")
        return audio_track
    # like ffmpeg's default pick: most channels, then lowest index
    return max(range(len(audio)), key=lambda i: (int(audio[i].get("channels") or 0), -i))


def extract_wav(src: Path, dest: Path, track: int) -> None:
    p = run(["ffmpeg", "-y", "-hide_banner", "-loglevel", "error", "-i", str(src),
             "-map", f"0:a:{track}", "-vn", "-ac", "1", "-ar", "16000",
             "-c:a", "pcm_s16le", str(dest)])
    if p.returncode != 0:
        raise SystemExit(f"ffmpeg failed: {p.stderr.strip()[:400]}")


def peak_dbfs(wav_path: Path) -> float:
    import array
    peak = 0
    with wave.open(str(wav_path), "rb") as w:
        while frames := w.readframes(1 << 16):
            samples = array.array("h", frames)
            peak = max(peak, max(samples), -min(samples))
    if peak == 0:
        return float("-inf")
    return 20 * math.log10(peak / 32768)


def device_line(stderr: str) -> str:
    for line in stderr.splitlines():
        if "using device" in line:
            return line.strip()
    return ""


# ---------------------------------------------------------------- vocab

def _read_gguf_string_array(model_path: Path, key: str) -> list[str] | None:
    """Minimal GGUF metadata reader for one string-array key."""
    import struct
    with open(model_path, "rb") as f:
        def u32() -> int:
            return struct.unpack("<I", f.read(4))[0]

        def u64() -> int:
            return struct.unpack("<Q", f.read(8))[0]

        def s() -> str:
            return f.read(u64()).decode("utf-8", "replace")

        sizes = {0: 1, 1: 1, 2: 2, 3: 2, 4: 4, 5: 4, 6: 4, 7: 1, 10: 8, 11: 8, 12: 8}

        def skip(kind: int) -> None:
            if kind == 8:
                f.read(u64())
            elif kind == 9:
                at, ac = u32(), u64()
                for _ in range(ac):
                    skip(at)
            else:
                f.read(sizes[kind])

        if f.read(4) != b"GGUF":
            return None
        u32()  # version
        u64()  # tensor count
        nkv = u64()
        for _ in range(nkv):
            k, vt = s(), u32()
            if k == key and vt == 9:
                at, ac = u32(), u64()
                if at == 8:
                    return [s() for _ in range(ac)]
                for _ in range(ac):
                    skip(at)
                return None
            skip(vt)
    return None


def load_vocab(model_path: Path) -> list[str] | None:
    """Tokenizer pieces straight from the model GGUF (cached next to it)."""
    cache = Path(str(model_path) + ".vocab.json")
    if cache.exists():
        try:
            return json.loads(cache.read_text(encoding="utf-8"))
        except Exception:
            pass
    pieces = _read_gguf_string_array(model_path, "parakeet.tokenizer.pieces")
    if pieces:
        try:
            cache.write_text(json.dumps(pieces, ensure_ascii=False), encoding="utf-8")
        except Exception:
            pass
    return pieces


def clean_piece(p: str) -> str:
    return p.replace("\u2581", " ")


# ---------------------------------------------------------------- engines

def run_asr(cli: Path, model: Path, wav: Path, lang: str | None) -> tuple[dict, str, float]:
    cmd = [str(cli), "transcribe", "--model", str(model), "--input", str(wav), "--json"]
    if lang:
        cmd += ["--lang", lang]
    t0 = time.time()
    p = run(cmd)
    dt = time.time() - t0
    if p.returncode != 0:
        raise SystemExit(f"parakeet-cli failed ({p.returncode}): {p.stderr.strip()[:500]}")
    try:
        data = json.loads(p.stdout)
    except json.JSONDecodeError:
        raise SystemExit(f"invalid JSON from parakeet-cli: {p.stdout[:300]}")
    return data, p.stderr, dt


def run_scene(cli: Path, asr_model: Path, diar_model: Path, wav: Path) -> tuple[list[dict] | None, str]:
    """Fused ASR + diarization (parakeet-cli scene --json; JSONL snapshots)."""
    cmd = [str(cli), "scene", "--model", str(asr_model), "--diar", str(diar_model),
           "--input", str(wav), "--json"]
    p = run(cmd)
    if p.returncode != 0:
        return None, p.stderr
    words, seen = [], set()
    for line in p.stdout.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            obj = json.loads(line)
        except json.JSONDecodeError:
            continue
        for w in obj.get("words", []) or []:
            txt = (w.get("text") or "").strip()
            if not txt or "start" not in w or "end" not in w:
                continue
            key = (round(float(w["start"]), 3), round(float(w["end"]), 3), txt)
            if key in seen:
                continue
            seen.add(key)
            words.append({"text": txt, "start": float(w["start"]), "end": float(w["end"]),
                          "conf": w.get("conf"), "speaker": w.get("speaker")})
    if not words:
        return None, p.stderr
    words.sort(key=lambda w: w["start"])
    return words, p.stderr


def run_diarization(diarize_exe: Path, diar_model: Path, wav: Path) -> list[dict] | None:
    """diarize <gguf> <wav> -> {"speakers":N,"segments":[{speaker,start,end}]}"""
    p = run([str(diarize_exe), str(diar_model), str(wav)])
    out = p.stdout.strip()
    if p.returncode != 0 or not out:
        return None
    try:
        data = json.loads(out.splitlines()[-1])
    except json.JSONDecodeError:
        return None
    norm = []
    for s in data.get("segments", []):
        if "start" not in s or "end" not in s:
            continue
        spk = s.get("speaker", 0)
        norm.append({"start": float(s["start"]), "end": float(s["end"]),
                     "speaker": int(spk) if str(spk).isdigit() else spk})
    return norm or None


def normalize_asr_words(parsed: dict) -> list[dict]:
    out = []
    for w in parsed.get("words", []):
        txt = (w.get("w") or "").strip()
        if not txt or "start" not in w or "end" not in w:
            continue
        out.append({"text": txt, "start": float(w["start"]), "end": float(w["end"]),
                    "conf": w.get("conf"), "speaker": None})
    return out


def assign_speakers(words: list[dict], segs: list[dict]) -> None:
    """Set w['speaker']: max overlap, else nearest segment within 0.5 s."""
    for w in words:
        ws, we = float(w["start"]), float(w["end"])
        if we < ws:
            we = ws
        best, best_ov = None, 0.0
        for s in segs:
            ov = min(we, s["end"]) - max(ws, s["start"])
            if ov > best_ov:
                best, best_ov = s, ov
        if best is None and we <= ws:
            for s in segs:  # zero-duration token exactly on a boundary
                if s["start"] <= ws <= s["end"]:
                    best = s
                    break
        if best is None:
            nearest, nd = None, 1e9
            for s in segs:
                d = min(abs(ws - s["end"]), abs(s["start"] - we))
                if d < nd:
                    nearest, nd = s, d
            if nearest is not None and nd <= 0.5:
                best = nearest
        w["speaker"] = best["speaker"] if best is not None else None


# ---------------------------------------------------------------- artifacts

PUNCT_FIX = [(" ,", ","), (" .", "."), (" ?", "?"), (" !", "!"), (" ;", ";"), (" :", ":")]


def fix_text(t: str) -> str:
    for a, b in PUNCT_FIX:
        t = t.replace(a, b)
    return t


def words_to_scribe(norm_words: list[dict], segs: list[dict] | None = None) -> dict:
    words = [dict(w) for w in norm_words]
    if segs:
        assign_speakers(words, segs)

    out_words: list[dict] = []
    prev_end = None
    for w in words:
        if prev_end is not None and w["start"] - prev_end > 0.02:
            out_words.append({"text": "", "start": round(prev_end, 3),
                              "end": round(w["start"], 3), "type": "spacing"})
        entry = {"text": w["text"], "start": round(w["start"], 3), "end": round(w["end"], 3),
                 "type": "word"}
        if w.get("speaker") is not None:
            entry["speaker_id"] = f"speaker_{w['speaker']}"
        if w.get("conf") is not None:
            entry["confidence"] = w["conf"]
        out_words.append(entry)
        prev_end = w["end"]

    text = fix_text(" ".join(w["text"] for w in words)).strip()
    return {"text": text, "words": out_words}


def fmt_srt(t: float) -> str:
    ms = int(round(t * 1000))
    h, ms = divmod(ms, 3600000)
    m, ms = divmod(ms, 60000)
    s, ms = divmod(ms, 1000)
    return f"{h:02d}:{m:02d}:{s:02d},{ms:03d}"


def fmt_vtt(t: float) -> str:
    return fmt_srt(t).replace(",", ".")


def fmt_clock(t: float, with_hours: bool) -> str:
    s = int(t)
    h, s = divmod(s, 3600)
    m, s = divmod(s, 60)
    return f"{h:02d}:{m:02d}:{s:02d}" if with_hours else f"{m:02d}:{s:02d}"


def group_cues(words: list[dict], max_chars: int = 46, max_dur: float = 7.0,
               gap: float = 0.8) -> list[dict]:
    cues, cur = [], []
    for w in words:
        if w.get("type") != "word":
            continue
        if not cur:
            cur = [w]
            continue
        gap_now = w["start"] - cur[-1]["end"]
        text_len = sum(len(x["text"]) + 1 for x in cur)
        sentence = cur[-1]["text"][-1:] in ".?!"
        if gap_now >= gap or (w["end"] - cur[0]["start"]) > max_dur or text_len >= max_chars \
                or (sentence and text_len > 18):
            cues.append(cur)
            cur = [w]
        else:
            cur.append(w)
    if cur:
        cues.append(cur)
    return [{"start": c[0]["start"], "end": c[-1]["end"],
             "text": fix_text(" ".join(x["text"] for x in c)),
             "speaker_id": c[0].get("speaker_id")} for c in cues]


def write_srt(cues: list[dict], path: Path, speaker_labels: bool = False) -> None:
    lines = []
    for i, c in enumerate(cues, 1):
        prefix = ""
        if speaker_labels and c.get("speaker_id"):
            prefix = f"[{c['speaker_id'].replace('speaker_', 'S')}] "
        lines += [str(i), f"{fmt_srt(c['start'])} --> {fmt_srt(c['end'])}", prefix + c["text"], ""]
    path.write_text("\n".join(lines), encoding="utf-8")


def write_vtt(cues: list[dict], path: Path, speaker_labels: bool = False) -> None:
    lines = ["WEBVTT", ""]
    for c in cues:
        prefix = ""
        if speaker_labels and c.get("speaker_id"):
            prefix = f"<v {c['speaker_id']}>"
        lines += [f"{fmt_vtt(c['start'])} --> {fmt_vtt(c['end'])}", prefix + c["text"], ""]
    path.write_text("\n".join(lines), encoding="utf-8")


def write_txt(scribe: dict, path: Path, duration: float) -> None:
    """Reading-focused transcript: context blocks, one timestamp per block."""
    words = [w for w in scribe["words"] if w["type"] == "word"]
    with_hours = duration >= 3600
    blocks: list[list[dict]] = []
    cur: list[dict] = []
    prev_end = None
    for w in words:
        gap = (w["start"] - prev_end) if prev_end is not None else 0.0
        if cur and (gap >= 2.0 or (gap >= 1.0 and cur[-1]["text"][-1:] in ".!?")):
            blocks.append(cur)
            cur = []
        cur.append(w)
        prev_end = w["end"]
    if cur:
        blocks.append(cur)

    lines = []
    for b in blocks:
        stamp = fmt_clock(b[0]["start"], with_hours)
        spk = b[0].get("speaker_id")
        prefix = f"{str(spk).replace('speaker_', 'Speaker ')}: " if spk else ""
        lines.append(f"[{stamp}] {prefix}{fix_text(' '.join(x['text'] for x in b))}")
    path.write_text("\n\n".join(lines) + "\n", encoding="utf-8")


def write_review(scribe: dict, path: Path, threshold: float) -> None:
    """Review report for agents: low-confidence words + repair guidance."""
    words = [w for w in scribe["words"] if w["type"] == "word"]
    flagged = [w for w in words if w.get("confidence") is not None and w["confidence"] < threshold]
    lines = [f"Transcription review (confidence threshold {threshold:.2f})", "",
             f"words: {len(words)} | flagged below threshold: {len(flagged)}"]
    if flagged:
        lines.append("")
        for w in flagged:
            lines.append(f"  [{fmt_srt(w['start'])[:12]}] {w['text']!r}  conf {w['confidence']:.2f}")
    lines += [
        "",
        "Agent guidance:",
        "1. Also read the full text yourself: a long passage in another language can bias the",
        "   decoder (drift), e.g. Portuguese words rendered as English near-homophones",
        "   ('reuniao' -> 'reunion'). Confidence alone does not catch every drift case.",
        "2. For a suspicious span, get a fresh-context transcription (decoder state reset):",
        '     python transcribe.py <media> --retranscribe "START-END"',
        "   and patch the final text with the fresh result, keeping the original timings.",
    ]
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def _retranscribe(cfg: dict, home: Path, cli: Path, asr_model: Path, src: Path, args) -> int:
    """Fresh-context transcription of one span (agent repair workflow)."""
    spec = args.retranscribe
    try:
        a_s, b_s = spec.split("-", 1)
        r_start, r_end = float(a_s), float(b_s)
    except Exception:
        raise SystemExit('--retranscribe expects "START-END" in seconds, e.g. "7.2-11.3"')
    if r_end <= r_start:
        raise SystemExit("--retranscribe: END must be greater than START")

    audio, duration = ffprobe_streams(src)
    track = pick_track(audio, args.audio_track)
    out_dir = (args.out or (src.parent / "transcripts")).resolve()
    out_dir.mkdir(parents=True, exist_ok=True)
    pad = 0.4
    s = max(0.0, r_start - pad)
    e = min(duration, r_end + pad) if duration else r_end + pad

    with tempfile.TemporaryDirectory(prefix="rt_") as tmp:
        full = Path(tmp) / "full.wav"
        extract_wav(src, full, track)
        cut = Path(tmp) / "cut.wav"
        p = run(["ffmpeg", "-y", "-hide_banner", "-loglevel", "error",
                 "-ss", f"{s:.3f}", "-i", str(full), "-t", f"{e - s:.3f}",
                 "-c:a", "pcm_s16le", str(cut)])
        if p.returncode != 0:
            raise SystemExit(f"ffmpeg cut failed: {p.stderr.strip()[:300]}")
        parsed, _stderr, dt = run_asr(cli, asr_model, cut, args.lang)

    words = []
    for w in parsed.get("words", []):
        txt = (w.get("w") or "").strip()
        if not txt:
            continue
        words.append({"text": txt, "start": round(float(w["start"]) + s, 3),
                      "end": round(float(w["end"]) + s, 3), "confidence": w.get("conf")})
    text = fix_text(" ".join(w["text"] for w in words))

    base = src.stem + ("" if track == 0 else f".track{track}")
    tag = f"{r_start:.2f}-{r_end:.2f}"
    jp = out_dir / f"{base}.rt_{tag}.json"
    tp = out_dir / f"{base}.rt_{tag}.txt"
    jp.write_text(json.dumps({
        "text": text,
        "words": words,
        "slice": {"requested_start": r_start, "requested_end": r_end,
                  "padded_start": round(s, 3), "padded_end": round(e, 3), "pad_s": pad},
        "note": "fresh-context transcription of one span; times are ABSOLUTE (original file)",
    }, ensure_ascii=False, indent=1), encoding="utf-8")
    tp.write_text(text + "\n", encoding="utf-8")
    if not args.quiet:
        log(f"retranscribe [{tag}]  ({dt:.1f}s):")
        log(f"  {text}")
        log(f"  {jp}")
    else:
        log(text)
    return 0


# ---------------------------------------------------------------- main

def main() -> int:
    ap = argparse.ArgumentParser(
        description="Local speech-to-text with word timestamps (Parakeet TDT 0.6B v3, Vulkan)")
    ap.add_argument("media", type=Path, help="audio or video file")
    ap.add_argument("--out", type=Path, default=None,
                    help="output directory (default: <media_parent>/transcripts)")
    ap.add_argument("--quant", default=None, choices=["f16", "q8_0"],
                    help="model precision (default: f16)")
    ap.add_argument("--lang", default=None,
                    help="language hint forwarded to the runtime (ignored by the v3 model)")
    ap.add_argument("--tokens", action="store_true",
                    help="JSON also carries token-level timestamps+confidences (text via model vocab)")
    ap.add_argument("--review-threshold", type=float, default=None,
                    help="confidence threshold for the review report (default 0.6)")
    ap.add_argument("--retranscribe", default=None, metavar="START-END",
                    help='fresh-context re-transcription of one span (seconds), e.g. "7.2-11.3"')
    ap.add_argument("--diarize", default=None, choices=["auto", "on", "off"],
                    help="speaker diarization (default: auto = on when available)")
    ap.add_argument("--audio-track", type=int, default=None,
                    help="zero-based audio track index (default: most channels)")
    ap.add_argument("--formats", default=None,
                    help="comma list of json,srt,txt,vtt (default: json,srt,txt)")
    ap.add_argument("--cli", default=None, help="parakeet-cli path override")
    ap.add_argument("--model", default=None, help="ASR model (.gguf) path override")
    ap.add_argument("--diar-model", default=None, help="diarization model path override")
    ap.add_argument("--force", action="store_true", help="ignore cache")
    ap.add_argument("--quiet", action="store_true")
    args = ap.parse_args()

    cfg = load_config()
    defaults = cfg.get("defaults") or {}
    quant = args.quant or defaults.get("quant", "f16")
    lang = args.lang if args.lang is not None else defaults.get("lang")
    formats = [f.strip() for f in (args.formats or ",".join(
        defaults.get("formats", ["json", "srt", "txt"]))).split(",") if f.strip()]
    unknown = [f for f in formats if f not in ("json", "srt", "txt", "vtt")]
    if unknown:
        raise SystemExit(f"unknown format(s): {', '.join(unknown)} (use json,srt,txt,vtt)")
    # --diarize default: config defaults.diarize (true/false) or "auto"
    diarize_arg = args.diarize
    if diarize_arg is None:
        d = defaults.get("diarize")
        diarize_arg = "on" if d is True else ("off" if d is False else "auto")
    rt = args.review_threshold if args.review_threshold is not None else float(
        defaults.get("review_threshold", 0.6))

    src = args.media.resolve()
    if not src.exists():
        raise SystemExit(f"file not found: {src}")

    home = resolve_home(cfg)
    cli = resolve_cli(cfg, home, args.cli)
    asr_model = resolve_model(cfg, home, quant, args.model)

    if args.retranscribe:
        return _retranscribe(cfg, home, cli, asr_model, src, args)
    diar_model = resolve_diar_model(cfg, home, args.diar_model)
    diarize_exe = resolve_diarize(cfg, home, cli, None) if (diar_model or diarize_arg == "on") else None
    want_diar = diarize_arg != "off"
    can_diar = want_diar and diar_model is not None and diarize_exe is not None
    if diarize_arg == "on" and not can_diar:
        raise SystemExit(
            "diarization requested but unavailable:\n"
            f"  diarize binary: {diarize_exe or 'not found'}\n"
            f"  diarization model: {diar_model or 'not found'}\n"
            "  See README (Optional: speaker diarization)."
        )

    out_dir = (args.out or (src.parent / "transcripts")).resolve()
    out_dir.mkdir(parents=True, exist_ok=True)
    meta_dir = out_dir / ".meta"
    meta_dir.mkdir(parents=True, exist_ok=True)
    stem = src.stem

    audio, duration = ffprobe_streams(src)
    track = pick_track(audio, args.audio_track)
    suffix = "" if track == 0 else f".track{track}"
    base = stem + suffix
    src_sha = sha256_file(src)

    expected: dict[str, Path] = {}
    if "json" in formats:
        expected["json"] = out_dir / f"{base}.json"
    if "srt" in formats:
        expected["srt"] = out_dir / f"{base}.srt"
    if "txt" in formats:
        expected["txt"] = out_dir / f"{base}.txt"
    if "vtt" in formats:
        expected["vtt"] = out_dir / f"{base}.vtt"
    expected["review"] = out_dir / f"{base}.review.txt"
    mpath = meta_dir / f"{base}.meta.json"

    # cache: meta matches and every requested artifact exists
    if not args.force and mpath.exists() and all(p.exists() for p in expected.values()):
        try:
            old = json.loads(mpath.read_text(encoding="utf-8"))
            if (old.get("source_sha256") == src_sha and old.get("quant") == quant
                    and old.get("lang") == lang and old.get("audio_track") == track
                    and old.get("diarized") == bool(can_diar)
                    and old.get("tokens") == bool(args.tokens)):
                if not args.quiet:
                    log(f"cached: {expected.get('json') or mpath}")
                return 0
        except Exception:
            pass

    if not args.quiet:
        log(f"source: {src.name}  ({duration:.1f}s, {len(audio)} track(s), using #{track})")
        log(f"runtime: {cli.name}   model: {asr_model.name}   quant: {quant}")

    with tempfile.TemporaryDirectory(prefix="transcribe_") as tmp:
        wav = Path(tmp) / f"{base}.16k.wav"
        extract_wav(src, wav, track)
        peak = peak_dbfs(wav)
        if peak < -60.0:
            raise SystemExit(f"track #{track} is silent (peak {peak:.1f} dBFS)")
        if not args.quiet:
            log(f"audio: mono 16k  peak {peak:.1f} dBFS")

        device = ""
        diar_mode = None
        segs = None
        norm_words = None
        raw_tokens: list = []
        asr_dt = 0.0

        if can_diar and not args.tokens:
            # scene mode does not export token-level data; --tokens uses ASR + post diarization
            t0 = time.time()
            scene_words, scene_err = run_scene(cli, asr_model, diar_model, wav)
            scene_dt = time.time() - t0
            if scene_words:
                norm_words = scene_words
                diar_mode = "scene"
                asr_dt = scene_dt
                device = device_line(scene_err)
                if not args.quiet:
                    spk = sorted({w["speaker"] for w in norm_words if w.get("speaker") is not None})
                    log(f"scene (ASR + diarization): {scene_dt:.1f}s for {duration:.1f}s "
                        f"({duration / max(scene_dt, 1e-6):.1f}x), {len(norm_words)} words, "
                        f"{len(spk)} speaker(s)  {device}")

        if norm_words is None:
            parsed, stderr, asr_dt = run_asr(cli, asr_model, wav, lang)
            device = device_line(stderr)
            if not args.quiet:
                log(f"ASR: {asr_dt:.1f}s for {duration:.1f}s "
                    f"({duration / max(asr_dt, 1e-6):.1f}x)  {device}")
            norm_words = normalize_asr_words(parsed)
            raw_tokens = parsed.get("tokens") or []
            if can_diar:
                t0 = time.time()
                segs = run_diarization(diarize_exe, diar_model, wav)
                if segs:
                    diar_mode = "post"
                    spk = sorted({s["speaker"] for s in segs})
                    if not args.quiet:
                        log(f"diarization (post): {time.time() - t0:.1f}s, {len(segs)} turns, "
                            f"{len(spk)} speaker(s)")

    scribe = words_to_scribe(norm_words, segs)

    written: list[Path] = []
    cues = group_cues(scribe["words"])
    if "json" in formats:
        rich = {
            "text": scribe["text"],
            "words": scribe["words"],
            "meta": {
                "source": str(src),
                "duration_s": round(duration, 3),
                "model": asr_model.name,
                "quant": quant,
                "diarized": diar_mode is not None,
                "language_hint": lang,
                "review_threshold": rt,
            },
        }
        if args.tokens:
            vocab = load_vocab(asr_model)
            tokens_out = []
            for tok in raw_tokens:
                tid = tok.get("id")
                txt = None
                if vocab and isinstance(tid, int) and 0 <= tid < len(vocab):
                    txt = clean_piece(vocab[tid])
                tokens_out.append({"id": tid, "text": txt, "start": tok.get("t"),
                                   "confidence": tok.get("conf")})
            rich["tokens"] = tokens_out
            if not vocab:
                log("warning: tokenizer pieces not found in the GGUF; tokens carry ids only")
        expected["json"].write_text(json.dumps(rich, ensure_ascii=False, indent=1), encoding="utf-8")
        written.append(expected["json"])
    if "srt" in formats:
        write_srt(cues, expected["srt"])
        written.append(expected["srt"])
    if "txt" in formats:
        write_txt(scribe, expected["txt"], duration)
        written.append(expected["txt"])
    if "vtt" in formats:
        write_vtt(cues, expected["vtt"])
        written.append(expected["vtt"])
    write_review(scribe, expected["review"], rt)
    written.append(expected["review"])

    meta = {
        "source": str(src),
        "source_name": src.name,
        "source_bytes": src.stat().st_size,
        "source_sha256": src_sha,
        "duration_s": round(duration, 3),
        "audio_track": track,
        "quant": quant,
        "lang": lang,
        "diarized": diar_mode is not None,
        "diar_mode": diar_mode,
        "diar_model": diar_model.name if diar_mode else None,
        "cli": str(cli),
        "asr_seconds": round(asr_dt, 2),
        "rtf": round(duration / max(asr_dt, 1e-6), 2),
        "words": len([w for w in scribe["words"] if w["type"] == "word"]),
        "tokens": bool(args.tokens),
        "review_threshold": rt,
        "formats": formats,
        "created_utc": datetime.now(timezone.utc).isoformat(),
    }
    mpath.write_text(json.dumps(meta, ensure_ascii=False, indent=1), encoding="utf-8")
    written.append(mpath)

    if not args.quiet:
        log(f"ok: {meta['words']} words ->")
        for p in written:
            log(f"  {p}")
        log(f"text: {scribe['text'][:160]}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except SystemExit:
        raise
    except KeyboardInterrupt:
        raise SystemExit(130)
