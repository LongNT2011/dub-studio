//! ffmpeg/ffprobe-обёртки для analyze. Порт нужных кусков dubengine/media.py: probe (длительность,
//! видеопоток, fps, кодек) и extract_audio -> wav 16k mono. Тяжёлого ничего: только вызовы бинарей.

use serde_json::Value;
use std::path::Path;
use std::process::Command;

#[cfg(windows)]
const FFMPEG: &str = "ffmpeg.exe";
#[cfg(windows)]
const FFPROBE: &str = "ffprobe.exe";
#[cfg(not(windows))]
const FFMPEG: &str = "ffmpeg";
#[cfg(not(windows))]
const FFPROBE: &str = "ffprobe";

/// Сводка probe: длительность/размер/fps/кодек первого видеопотока. Зеркало api._meta().
#[derive(Debug, Clone, Default)]
pub struct MediaMeta {
    pub duration: f64,
    pub width: i64,
    pub height: i64,
    pub fps: f64,
    pub src_codec: String,
}

fn parse_fps(r: &str) -> f64 {
    // r_frame_rate вида "30000/1001".
    let mut it = r.split('/');
    match (it.next(), it.next()) {
        (Some(n), Some(d)) => {
            let n: f64 = n.parse().unwrap_or(0.0);
            let d: f64 = d.parse().unwrap_or(0.0);
            if d != 0.0 {
                n / d
            } else {
                0.0
            }
        }
        _ => 0.0,
    }
}

/// ffprobe -show_format -show_streams (json) -> MediaMeta. Ошибка если нет видеопотока/длительности.
pub fn probe(input: &Path) -> Result<MediaMeta, String> {
    let out = Command::new(FFPROBE)
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(input)
        .output()
        .map_err(|e| format!("ffprobe запуск не удался: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "ffprobe вернул код {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let v: Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("ffprobe json: {e}"))?;
    let streams = v
        .get("streams")
        .and_then(|s| s.as_array())
        .ok_or("ffprobe: нет streams")?;
    let vstream = streams
        .iter()
        .find(|s| s.get("codec_type").and_then(|t| t.as_str()) == Some("video"))
        .ok_or("во входе нет видеопотока")?;
    let duration = v
        .get("format")
        .and_then(|f| f.get("duration"))
        .and_then(|d| d.as_str())
        .and_then(|d| d.parse::<f64>().ok())
        .ok_or("не удалось определить длительность")?;
    let width = vstream.get("width").and_then(|w| w.as_i64()).unwrap_or(0);
    let height = vstream.get("height").and_then(|h| h.as_i64()).unwrap_or(0);
    let fps = vstream
        .get("r_frame_rate")
        .and_then(|r| r.as_str())
        .map(parse_fps)
        .unwrap_or(0.0);
    let src_codec = vstream
        .get("codec_name")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();
    Ok(MediaMeta {
        duration,
        width,
        height,
        fps,
        src_codec,
    })
}

/// Извлечь аудиодорожку в WAV 16 кГц mono (pcm_s16le) — вход ASR. Порт media.to_16k_mono/extract_audio
/// (объединённо: сразу 16k/mono, т.к. дальше в порту нет separation-стадии). Если у видео нет аудио —
/// ffmpeg вернёт ошибку, которую пробрасываем.
pub fn extract_wav_16k_mono(input: &Path, out_wav: &Path) -> Result<(), String> {
    let status = Command::new(FFMPEG)
        .arg("-y")
        .arg("-i")
        .arg(input)
        .args([
            "-vn", "-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le", "-f", "wav",
        ])
        .arg(out_wav)
        .output()
        .map_err(|e| format!("ffmpeg запуск не удался: {e}"))?;
    if !status.status.success() {
        return Err(format!(
            "ffmpeg extract_audio код {:?}: {}",
            status.status.code(),
            String::from_utf8_lossy(&status.stderr)
        ));
    }
    if !out_wav.is_file() {
        return Err("ffmpeg не создал wav".into());
    }
    Ok(())
}

// ─── Рендер-хелперы (порт media.py: extract_audio/duration/time_stretch/mix/mux/trim) ─────────

fn run_ff(args: &[&std::ffi::OsStr]) -> Result<(), String> {
    let out = Command::new(FFMPEG)
        .args(args)
        .output()
        .map_err(|e| format!("ffmpeg запуск: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: String = err.chars().rev().take(1500).collect::<String>().chars().rev().collect();
        return Err(format!("ffmpeg код {:?}:\n{tail}", out.status.code()));
    }
    Ok(())
}

use std::ffi::OsStr;

/// Извлечь аудио в WAV sr/ac (порт media.extract_audio). Для сепарации: sr=44100, ac=2.
pub fn extract_audio(video: &Path, out_wav: &Path, sr: u32, ac: u32) -> Result<(), String> {
    let sr = sr.to_string();
    let ac = ac.to_string();
    run_ff(&[
        OsStr::new("-y"), OsStr::new("-i"), video.as_os_str(),
        OsStr::new("-vn"), OsStr::new("-ac"), OsStr::new(&ac),
        OsStr::new("-ar"), OsStr::new(&sr), out_wav.as_os_str(),
    ])
}

/// WAV/медиа -> 16k mono (порт media.to_16k_mono).
pub fn to_16k_mono(src: &Path, dst: &Path) -> Result<(), String> {
    run_ff(&[
        OsStr::new("-y"), OsStr::new("-i"), src.as_os_str(),
        OsStr::new("-vn"), OsStr::new("-ac"), OsStr::new("1"),
        OsStr::new("-ar"), OsStr::new("16000"), dst.as_os_str(),
    ])
}

/// Длительность файла в секундах (ffprobe format.duration). Порт media.duration.
pub fn duration(path: &Path) -> Result<f64, String> {
    let out = Command::new(FFPROBE)
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "default=nw=1:nk=1"])
        .arg(path)
        .output()
        .map_err(|e| format!("ffprobe запуск: {e}"))?;
    if !out.status.success() {
        return Err(format!("ffprobe duration код {:?}", out.status.code()));
    }
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<f64>()
        .map_err(|e| format!("duration parse: {e}"))
}

/// atempo-цепочка для factor вне [0.5,2.0] (порт media._atempo_chain).
fn atempo_chain(factor: f64) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut f = factor;
    while f > 2.0 {
        parts.push("atempo=2.0".into());
        f /= 2.0;
    }
    while f < 0.5 {
        parts.push("atempo=0.5".into());
        f /= 0.5;
    }
    parts.push(format!("atempo={:.6}", f));
    parts.join(",")
}

/// factor>1 ускоряет (укорачивает); <1 замедляет. Порт media.time_stretch.
pub fn time_stretch(src: &Path, dst: &Path, factor: f64) -> Result<(), String> {
    let chain = atempo_chain(factor);
    run_ff(&[
        OsStr::new("-y"), OsStr::new("-i"), src.as_os_str(),
        OsStr::new("-filter:a"), OsStr::new(&chain), dst.as_os_str(),
    ])
}

/// Свести дубль-вокал поверх инструментала (музыка приглушена music_gain=0.45). Порт media.mix.
/// Нормализуем оба входа к stereo (aformat) ДО amix: наш mono-дубляж (hound) несёт нестандартный
/// channel layout "1 channels (FL)", который AAC-энкодер отвергает (-22). aformat=cl=stereo снимает это.
pub fn mix(voice: &Path, music: &Path, out: &Path) -> Result<(), String> {
    let fc = "[0:a]aformat=channel_layouts=stereo[v];\
              [1:a]aformat=channel_layouts=stereo,volume=0.45[m];\
              [v][m]amix=inputs=2:duration=longest:dropout_transition=0[a]";
    run_ff(&[
        OsStr::new("-y"), OsStr::new("-i"), voice.as_os_str(), OsStr::new("-i"), music.as_os_str(),
        OsStr::new("-filter_complex"), OsStr::new(fc), OsStr::new("-map"), OsStr::new("[a]"),
        OsStr::new("-c:a"), OsStr::new("aac"), OsStr::new("-b:a"), OsStr::new("192k"), out.as_os_str(),
    ])
}

/// Финальная нормализация программы по EBU R128 (ffmpeg loudnorm): интегральная громкость к I LUFS
/// + true-peak лимитер к TP dBTP. Ставится последним шагом на смиксованную дорожку.
pub fn loudnorm(src: &Path, dst: &Path, i: f64, tp: f64, lra: f64) -> Result<(), String> {
    let af = format!("loudnorm=I={i}:TP={tp}:LRA={lra}");
    run_ff(&[
        OsStr::new("-y"), OsStr::new("-i"), src.as_os_str(),
        OsStr::new("-af"), OsStr::new(&af),
        OsStr::new("-c:a"), OsStr::new("aac"), OsStr::new("-b:a"), OsStr::new("192k"), dst.as_os_str(),
    ])
}

/// Смуксить видео (copy) + аудио (aac). БЕЗ -shortest (выход по длиннейшему потоку). Порт media.mux.
/// -af aformat=cl=stereo нормализует channel layout (mono-дубляж от hound = "1 channels (FL)", AAC
/// его отвергает -22); на уже-стерео входе это no-op.
pub fn mux(video: &Path, audio: &Path, out: &Path) -> Result<(), String> {
    run_ff(&[
        OsStr::new("-y"), OsStr::new("-i"), video.as_os_str(), OsStr::new("-i"), audio.as_os_str(),
        OsStr::new("-map"), OsStr::new("0:v:0"), OsStr::new("-map"), OsStr::new("1:a:0"),
        OsStr::new("-af"), OsStr::new("aformat=channel_layouts=stereo"),
        OsStr::new("-c:v"), OsStr::new("copy"), OsStr::new("-c:a"), OsStr::new("aac"), out.as_os_str(),
    ])
}

/// Вырезать [start,end] в mono 16k (для клон-референса). Порт media.trim.
pub fn trim(src: &Path, dst: &Path, start: f64, end: f64) -> Result<(), String> {
    let ss = format!("{:.3}", start);
    let to = format!("{:.3}", end);
    run_ff(&[
        OsStr::new("-y"), OsStr::new("-ss"), OsStr::new(&ss), OsStr::new("-to"), OsStr::new(&to),
        OsStr::new("-i"), src.as_os_str(), OsStr::new("-ac"), OsStr::new("1"),
        OsStr::new("-ar"), OsStr::new("16000"), dst.as_os_str(),
    ])
}
