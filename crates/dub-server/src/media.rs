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
    // Видеопоток ОПЦИОНАЛЕН: чистый аудио-вход (WAV/mp3/…) поддерживается в аудио-режиме (без видео).
    // Нет видео -> width/height/fps=0 (сигнал audio-only), кодек берём из аудиопотока.
    let vstream = streams
        .iter()
        .find(|s| s.get("codec_type").and_then(|t| t.as_str()) == Some("video"));
    let astream = streams
        .iter()
        .find(|s| s.get("codec_type").and_then(|t| t.as_str()) == Some("audio"));
    if vstream.is_none() && astream.is_none() {
        return Err("во входе нет ни видео-, ни аудиопотока".to_string());
    }
    let duration = v
        .get("format")
        .and_then(|f| f.get("duration"))
        .and_then(|d| d.as_str())
        .and_then(|d| d.parse::<f64>().ok())
        .or_else(|| {
            // некоторые WAV не имеют format.duration -> берём из аудиопотока
            astream
                .and_then(|s| s.get("duration"))
                .and_then(|d| d.as_str())
                .and_then(|d| d.parse::<f64>().ok())
        })
        .ok_or("не удалось определить длительность")?;
    let width = vstream.and_then(|s| s.get("width")).and_then(|w| w.as_i64()).unwrap_or(0);
    let height = vstream.and_then(|s| s.get("height")).and_then(|h| h.as_i64()).unwrap_or(0);
    let fps = vstream
        .and_then(|s| s.get("r_frame_rate"))
        .and_then(|r| r.as_str())
        .map(parse_fps)
        .unwrap_or(0.0);
    let src_codec = vstream
        .or(astream)
        .and_then(|s| s.get("codec_name"))
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

/// Свести дубль-вокал поверх фона. МУЗЫКУ НЕ ГЛУШИМ (прямой приказ юзера, многократно): вокал уже вырезан
/// сепарацией, поэтому инструментал = чистый реальный фон и звучит в ПОЛНЫЙ уровень (1.0) — дублированный
/// голос заменяет вырезанный вокал, фон остаётся как в оригинале. amix normalize=0 НЕ делит входы пополам
/// (питон-дефолт=1 занизил бы фон ~в 0.5). aformat=cl=stereo снимает нестандартный mono layout hound-дубляжа
/// (уровень не меняет). Пики/итоговую громкость держит финальный loudnorm на смиксованной дорожке.
pub fn mix(voice: &Path, music: &Path, out: &Path) -> Result<(), String> {
    let fc = "[0:a]aformat=channel_layouts=stereo[v];\
              [1:a]aformat=channel_layouts=stereo[m];\
              [v][m]amix=inputs=2:duration=longest:dropout_transition=0:normalize=0[a]";
    run_ff(&[
        OsStr::new("-y"), OsStr::new("-i"), voice.as_os_str(), OsStr::new("-i"), music.as_os_str(),
        OsStr::new("-filter_complex"), OsStr::new(fc), OsStr::new("-map"), OsStr::new("[a]"),
        OsStr::new("-c:a"), OsStr::new("aac"), OsStr::new("-b:a"), OsStr::new("192k"), out.as_os_str(),
    ])
}

/// Сведение диалог+фон с САЙДЧЕЙН-ДАКИНГОМ — проф. практика дубляжа: компрессор приглушает фон
/// ТОЛЬКО пока звучит голос (attack 150мс / release 600мс — плавные подныривания), в паузах фон
/// живёт ровно 1:1 — атмосфера НЕ гробится статичным резом (требование юзера: «дубляж громче фона,
/// но фон не гробить»). Замер: фон мультика -17.1 LUFS ≈ уровню голоса — без дакинга речь тонет.
/// threshold 0.02 / ratio 8 ≈ -6..-9 дБ фону под фразой. Порядок sidechaincompress: [фон][ключ-голос].
pub fn mix_ducked(voice: &Path, music: &Path, out: &Path) -> Result<(), String> {
    let fc = "[0:a]aformat=channel_layouts=stereo,asplit=2[v][vkey];\
              [1:a]aformat=channel_layouts=stereo[m];\
              [m][vkey]sidechaincompress=threshold=0.02:ratio=8:attack=150:release=600:makeup=1[bg];\
              [v][bg]amix=inputs=2:duration=longest:dropout_transition=0:normalize=0[a]";
    run_ff(&[
        OsStr::new("-y"), OsStr::new("-i"), voice.as_os_str(), OsStr::new("-i"), music.as_os_str(),
        OsStr::new("-filter_complex"), OsStr::new(fc), OsStr::new("-map"), OsStr::new("[a]"),
        OsStr::new("-c:a"), OsStr::new("aac"), OsStr::new("-b:a"), OsStr::new("192k"), out.as_os_str(),
    ])
}

/// Речевой блок на таймлайне [start,end] — слитые по паузе <1.6с реплики дубляжа. Границы берутся из
/// ФАКТИЧЕСКИ уложенных сегментов (onset + длительность fit-файла), а не из сырых proj.segments.
#[derive(Clone, Copy, Debug)]
pub struct SpeechBlock {
    pub start: f64,
    pub end: f64,
}

// Параметры детерминированной огибающей дакинга (проф.практика Premiere Auto-Ducking / Descript):
const DUCK_GAIN: f64 = 0.25; // −12 дБ под речью
const DUCK_PREROLL: f64 = 0.08; // fade-down стартует за 0.08с ДО блока
const DUCK_FADE_DOWN: f64 = 0.10; // длина спуска
const DUCK_HOLD: f64 = 0.30; // держим приглушение 0.30с ПОСЛЕ конца блока
const DUCK_FADE_UP: f64 = 0.40; // длина подъёма

/// Собрать выражение gain(t) для ffmpeg-фильтра `volume` из речевых блоков: 1.0 вне блоков, DUCK_GAIN
/// внутри, линейные фейды на краях (down за DUCK_PREROLL до start, up через DUCK_HOLD после end).
/// Детерминированно и с точными dB — компрессор (sidechaincompress) реагировал на мгновенную амплитуду
/// TTS и давал «качели» на микропаузах; здесь огибающая задана таймингами, а не сигналом.
/// Форма: gain(t) = 1 − (1−g)·Σ trap_i(t), где trap_i — трапеция блока i (clip(min(рампа-вниз,
/// рампа-вверх),0,1)). Блоки разведены паузой ≥1.6с > preroll+fade+hold+fade — трапеции не пересекаются,
/// сумма ≤ 1. Длина выражения O(N) по блокам (вложенные if давали O(2^N) — дубль prev в обеих ветках).
fn duck_volume_expr(blocks: &[SpeechBlock]) -> String {
    let g = DUCK_GAIN;
    if blocks.is_empty() {
        return "1".into();
    }
    // Трапеция блока: (t-ds)/fd растёт 0->1 на спуске, (ue-t)/fu убывает 1->0 на подъёме; между ними
    // обе ≥1 -> clip даёт полку 1 (полное приглушение). Вне [ds,ue] одна из рамп ≤0 -> clip даёт 0.
    let traps: Vec<String> = blocks
        .iter()
        .map(|b| {
            let ds = (b.start - DUCK_PREROLL).max(0.0); // старт спуска
            let us = b.end + DUCK_HOLD; // старт подъёма (после hold)
            let ue = us + DUCK_FADE_UP; // конец подъёма = снова 1.0
            format!(
                "clip(min((t-{ds:.3})/{fd:.3},({ue:.3}-t)/{fu:.3}),0,1)",
                fd = DUCK_FADE_DOWN,
                fu = DUCK_FADE_UP
            )
        })
        .collect();
    format!("1-{d:.4}*({sum})", d = 1.0 - g, sum = traps.join("+"))
}

/// Сведение диалог+фон с ДЕТЕРМИНИРОВАННОЙ ОГИБАЮЩЕЙ дакинга (#106). Фон приглушается по кусочно-линейной
/// огибающей громкости, построенной из ТОЧНЫХ таймингов речевых блоков (а не по мгновенной амплитуде
/// TTS, как sidechaincompress — тот давал «качели» на микропаузах внутри фраз). volume с выражением от t
/// (eval=frame). На длинных видео сотни блоков -> выражение большое: filtergraph пишем в файл через
/// `-filter_complex_script` (лимит CreateProcess 32767, паттерн из dub-captions/burn.rs). Порядок как в
/// mix: голос полным уровнем + фон по огибающей, amix normalize=0. Пусто блоков -> фон ровно 1.0.
pub fn mix_env(voice: &Path, music: &Path, blocks: &[SpeechBlock], out: &Path) -> Result<(), String> {
    let vol = duck_volume_expr(blocks);
    let fc = format!(
        "[0:a]aformat=channel_layouts=stereo[v];\
         [1:a]aformat=channel_layouts=stereo,volume='{vol}':eval=frame[bg];\
         [v][bg]amix=inputs=2:duration=longest:dropout_transition=0:normalize=0[a]"
    );
    // Граф — в файл: выражение огибающей на сотнях блоков раздувает cmdline за лимит CreateProcess.
    let script = out.with_extension("envfilter");
    std::fs::write(&script, &fc).map_err(|e| format!("env filter-скрипт: {e}"))?;
    run_ff(&[
        OsStr::new("-y"), OsStr::new("-i"), voice.as_os_str(), OsStr::new("-i"), music.as_os_str(),
        OsStr::new("-filter_complex_script"), script.as_os_str(),
        OsStr::new("-map"), OsStr::new("[a]"),
        OsStr::new("-c:a"), OsStr::new("aac"), OsStr::new("-b:a"), OsStr::new("192k"), out.as_os_str(),
    ])
}

/// Финальная нормализация программы по EBU R128 (ffmpeg loudnorm): интегральная громкость к I LUFS
/// + true-peak лимитер к TP dBTP. Решение юзера (best-practice, НЕ питон): ставится последним шагом на
/// смиксованную дорожку — держит целевую громкость соцсетей и ловит пики (пофразного клэмпа поэтому нет).
pub fn loudnorm(src: &Path, dst: &Path, i: f64, tp: f64, lra: f64) -> Result<(), String> {
    let af = format!("loudnorm=I={i}:TP={tp}:LRA={lra}");
    run_ff(&[
        OsStr::new("-y"), OsStr::new("-i"), src.as_os_str(),
        OsStr::new("-af"), OsStr::new(&af),
        OsStr::new("-c:a"), OsStr::new("aac"), OsStr::new("-b:a"), OsStr::new("192k"), dst.as_os_str(),
    ])
}

/// Усилить всю дорожку на `gain_db` dB (монтажный гейн, наша opt-in фича). Перекодирование в aac.
pub fn gain(src: &Path, dst: &Path, gain_db: f64) -> Result<(), String> {
    let af = format!("volume={gain_db}dB");
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
        OsStr::new("-map"), OsStr::new("0:v:0"), OsStr::new("-map"), OsStr::new("1:a:0?"),
        OsStr::new("-af"), OsStr::new("aformat=channel_layouts=stereo"),
        OsStr::new("-c:v"), OsStr::new("copy"), OsStr::new("-c:a"), OsStr::new("aac"), out.as_os_str(),
    ])
}

/// Сконвертировать аудио в PCM 16-bit WAV (стерео). Финальный формат аудио-режима (вход без видео):
/// пачка WAV -> пачка озвученных WAV. Лоссовость только от исходного mix (aac), сам WAV без потерь.
pub fn to_wav(src: &Path, dst: &Path) -> Result<(), String> {
    run_ff(&[
        OsStr::new("-y"), OsStr::new("-i"), src.as_os_str(),
        OsStr::new("-af"), OsStr::new("aformat=channel_layouts=stereo"),
        OsStr::new("-c:a"), OsStr::new("pcm_s16le"), dst.as_os_str(),
    ])
}

/// Вырезать [start,end] в mono @ sr Гц. Порт media.trim(..., sr=16000): реф-клипы 16к, keep-сплайс 24к.
pub fn trim(src: &Path, dst: &Path, start: f64, end: f64, sr: u32) -> Result<(), String> {
    let ss = format!("{:.3}", start);
    let to = format!("{:.3}", end);
    let ar = sr.to_string();
    run_ff(&[
        OsStr::new("-y"), OsStr::new("-ss"), OsStr::new(&ss), OsStr::new("-to"), OsStr::new(&to),
        OsStr::new("-i"), src.as_os_str(), OsStr::new("-ac"), OsStr::new("1"),
        OsStr::new("-ar"), OsStr::new(&ar), dst.as_os_str(),
    ])
}

// ─── Оконная нарезка для полнометражного пайплайна (#79) ──────────────────────────────────────────
// Новые хелперы (не трогают существующие): вырезать ОДНО окно вокала в отдельный WAV (RAM O(окна)),
// либо нарезать весь вокал segment-muxer'ом одним проходом. `-reset_timestamps 1` даёт локальный t=0 в
// каждом окне (чистый клип для BSRoformer/Sortformer/ASR) — обратный сдвиг window_offset делает
// вызывающий при сшивке диаризации/ASR (dub_asr::Window::offset). Хардненные флаги для длинных входов
// (BORROWINGS #19): +discardcorrupt / avoid_negative_ts / max_muxing_queue_size — против timestamp-drift
// и 'Too many packets buffered' на часовых файлах.

/// Вырезать окно [start,end) вокала в отдельный WAV @ sr Гц (mono). Локальный t=0 (accurate seek:
/// -ss ПОСЛЕ -i для точного реза по сэмплу). Для стадийной обработки одного окна — RAM O(окна).
// allow(dead_code): вызывается оркестратором оконного пайплайна (интеграция #79 идёт отдельно).
#[allow(dead_code)]
pub fn slice_window(src: &Path, dst: &Path, start: f64, end: f64, sr: u32) -> Result<(), String> {
    let ss = format!("{:.3}", start);
    let to = format!("{:.3}", end);
    let ar = sr.to_string();
    run_ff(&[
        OsStr::new("-y"),
        OsStr::new("-fflags"), OsStr::new("+discardcorrupt"),
        OsStr::new("-i"), src.as_os_str(),
        // accurate seek внутри уже открытого потока (после -i) — точная граница окна
        OsStr::new("-ss"), OsStr::new(&ss), OsStr::new("-to"), OsStr::new(&to),
        OsStr::new("-ac"), OsStr::new("1"), OsStr::new("-ar"), OsStr::new(&ar),
        OsStr::new("-avoid_negative_ts"), OsStr::new("make_zero"),
        OsStr::new("-c:a"), OsStr::new("pcm_s16le"),
        dst.as_os_str(),
    ])
}

/// Нарезать весь вокал на окна фикс. длины `win_sec` segment-muxer'ом за ОДИН проход (RAM O(окна),
/// не O(файла)). Файлы пишутся по шаблону `pattern` с `%03d` (например `win_%03d.wav`).
/// `-reset_timestamps 1` -> каждый сегмент стартует с t=0; window_offset = idx*win_sec прибавляет
/// вызывающий при ре-базинге. Дешёвый фолбэк к min-cut нарезке, когда важна только RAM-локальность.
// allow(dead_code): вызывается оркестратором оконного пайплайна (интеграция #79 идёт отдельно).
#[allow(dead_code)]
pub fn segment_wav(src: &Path, out_dir: &Path, pattern: &str, win_sec: f64, sr: u32) -> Result<(), String> {
    std::fs::create_dir_all(out_dir).map_err(|e| format!("mkdir {}: {e}", out_dir.display()))?;
    let seg_time = format!("{:.3}", win_sec.max(0.1));
    let ar = sr.to_string();
    let out_tpl = out_dir.join(pattern);
    run_ff(&[
        OsStr::new("-y"),
        OsStr::new("-fflags"), OsStr::new("+discardcorrupt"),
        OsStr::new("-i"), src.as_os_str(),
        OsStr::new("-ac"), OsStr::new("1"), OsStr::new("-ar"), OsStr::new(&ar),
        OsStr::new("-c:a"), OsStr::new("pcm_s16le"),
        OsStr::new("-f"), OsStr::new("segment"),
        OsStr::new("-segment_time"), OsStr::new(&seg_time),
        OsStr::new("-reset_timestamps"), OsStr::new("1"),
        OsStr::new("-max_muxing_queue_size"), OsStr::new("2048"),
        out_tpl.as_os_str(),
    ])
}

#[cfg(test)]
mod env_tests {
    use super::*;

    /// Референс-огибающая: та же кусочно-линейная функция, что генерит duck_volume_expr, но на Rust —
    /// проверяем ключевые точки (вне блока 1.0, в центре блока DUCK_GAIN, середины фейдов).
    fn gain_at(t: f64, blocks: &[SpeechBlock]) -> f64 {
        let g = DUCK_GAIN;
        for b in blocks {
            let ds = (b.start - DUCK_PREROLL).max(0.0);
            let de = ds + DUCK_FADE_DOWN;
            let us = b.end + DUCK_HOLD;
            let ue = us + DUCK_FADE_UP;
            if t >= ds && t < ue {
                return if t < de {
                    1.0 - (1.0 - g) * (t - ds) / DUCK_FADE_DOWN
                } else if t < us {
                    g
                } else {
                    g + (1.0 - g) * (t - us) / DUCK_FADE_UP
                };
            }
        }
        1.0
    }

    #[test]
    fn envelope_key_points() {
        let blocks = [SpeechBlock { start: 5.0, end: 8.0 }, SpeechBlock { start: 20.0, end: 22.0 }];
        // вне любого блока -> 1.0
        assert!((gain_at(0.0, &blocks) - 1.0).abs() < 1e-9);
        assert!((gain_at(15.0, &blocks) - 1.0).abs() < 1e-9);
        // центр блока -> полное приглушение
        assert!((gain_at(6.5, &blocks) - DUCK_GAIN).abs() < 1e-9);
        // до начала спуска (за preroll) ещё 1.0
        assert!((gain_at(5.0 - DUCK_PREROLL - 0.01, &blocks) - 1.0).abs() < 1e-9);
        // середина fade-down: между 1.0 и g
        let mid_down = gain_at(5.0 - DUCK_PREROLL + DUCK_FADE_DOWN / 2.0, &blocks);
        assert!(mid_down < 1.0 && mid_down > DUCK_GAIN);
        // hold после конца блока ещё приглушено
        assert!((gain_at(8.0 + DUCK_HOLD - 0.01, &blocks) - DUCK_GAIN).abs() < 1e-9);
    }

    #[test]
    fn empty_blocks_is_flat_one() {
        assert_eq!(duck_volume_expr(&[]), "1");
    }

    #[test]
    fn expr_mentions_each_block_boundaries() {
        let blocks = [SpeechBlock { start: 1.0, end: 2.0 }];
        let e = duck_volume_expr(&blocks);
        // спуск стартует за preroll (0.920), глубина 1-g (0.7500) присутствует
        assert!(e.contains("0.920"), "{e}");
        assert!(e.contains("0.7500"), "{e}");
    }

    #[test]
    fn expr_length_linear_in_blocks() {
        // Длина выражения растёт линейно по блокам (вложенные if давали O(2^N) и взрывали память).
        let blocks: Vec<SpeechBlock> = (0..300)
            .map(|i| SpeechBlock { start: i as f64 * 10.0, end: i as f64 * 10.0 + 5.0 })
            .collect();
        let e = duck_volume_expr(&blocks);
        assert!(e.len() < 60 * 300 + 64, "len={}", e.len());
    }
}
