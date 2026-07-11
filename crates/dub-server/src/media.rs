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
