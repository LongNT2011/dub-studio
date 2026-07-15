//! Порт dubengine/ctx_translate.py run() — единый Gemma-проход: (1) vision layout -> sub_style/sub_y/
//! titles/brands/captions; (2) vision scene-контекст; (3) audio-контекст (окна <=28с); (4) перевод ВСЕГО
//! транскрипта (+тайтлы) С полным vision+audio контекстом. Каждая фаза fail-safe: упавшая фаза даёт пустой
//! контекст, перевод всё равно случается. Промпты TP/AP перенесены ДОСЛОВНО.

use std::path::{Path, PathBuf};

use base64::Engine;
use regex::Regex;
use serde_json::{Map, Value};

use dub_llm::{strip_think, ChatClient, Message, Part, Sampling};

use crate::seg::Seg;
use crate::vision;
use crate::TranslateError;

/// _LANG из ctx_translate — код -> имя (для vision/перевода).
fn lang_name(code: &str) -> String {
    let m: std::collections::HashMap<&str, &str> = [
        ("ru", "Russian"), ("en", "English"), ("es", "Spanish"), ("fr", "French"),
        ("de", "German"), ("it", "Italian"), ("pt", "Portuguese"), ("ja", "Japanese"),
        ("ko", "Korean"), ("zh", "Chinese"),
    ]
    .into_iter()
    .collect();
    m.get(code.to_lowercase().as_str()).map(|s| s.to_string()).unwrap_or_else(|| code.to_string())
}

/// Результат ctx-перевода: заполненные segs + extra (vision/audio/scene) как в питоне.
pub struct CtxResult {
    /// sub_style / sub_y / titles(+tgt) / brands / captions / audio_context / scene_context.
    pub extra: Value,
}

/// Конфиг ctx-прохода — минимальные поля cfg питона, нужные тут.
pub struct CtxConfig {
    pub input: PathBuf,     // исходное видео
    pub work_dir: PathBuf,  // рабочий каталог (_ctx_kf.png)
    pub tgt_lang: String,
    pub vocals16: Option<PathBuf>, // вокал для audio-контекста (может отсутствовать в порту — separation в раунде 4)
    pub vh: f64,            // высота кадра
    pub total: f64,         // длительность
}

/// run — единый проход. rewrite=Some(instr) -> творческий ре-дубляж; None -> точный перевод.
/// llm — уже поднятый клиент к llama-server с mmproj. Пишет segs[i].tgt и возвращает extra.
pub fn run(
    llm: &ChatClient,
    cfg: &CtxConfig,
    segs: &mut [Seg],
    rewrite: Option<&str>,
    mut log: impl FnMut(&str),
) -> Result<CtxResult, TranslateError> {
    let tgt = lang_name(&cfg.tgt_lang);
    let tmp = cfg.work_dir.join("_ctx_kf.png");

    let mut extra = serde_json::json!({
        "sub_style": Value::Null, "sub_y": Value::Null, "titles": [], "captions": [],
        "brands": [], "audio_context": "", "scene_context": ""
    });

    // ── фаза 1: VISION layout ──────────────────────────────────────────────
    match vision::analyze_layout(llm, &cfg.input, &tmp, cfg.total, cfg.vh) {
        Ok(layout) => {
            extra["sub_style"] = layout.sub_style.clone().unwrap_or(Value::Null);
            extra["sub_y"] = layout.sub_y.map(|y| Value::from(y)).unwrap_or(Value::Null);
            extra["titles"] = Value::Array(layout.titles.clone());
            extra["captions"] = Value::Array(layout.captions.clone());
            extra["brands"] = Value::Array(layout.brands.clone());
            let tnames: Vec<String> = layout.titles.iter().filter_map(|t| t.get("text").and_then(|x| x.as_str()).map(String::from)).collect();
            let bnames: Vec<String> = layout.brands.iter().filter_map(|b| b.get("text").and_then(|x| x.as_str()).map(String::from)).collect();
            log(&format!("  ctx vision: sub_style={} titles={:?} brands={:?}", extra["sub_style"], tnames, bnames));
        }
        Err(e) => log(&format!("  ctx vision skipped: {e}")),
    }

    // ── фаза 2: VISION scene-контекст ──────────────────────────────────────
    match vision::scene_context(llm, &cfg.input, &tmp, cfg.total, &tgt) {
        Ok(sc) => extra["scene_context"] = Value::from(sc),
        Err(e) => log(&format!("  ctx scene skipped: {e}")),
    }

    // ── фаза 3: AUDIO-контекст (окна <=28с). Fail-safe: нет вокала / модель не умеет audio -> пусто ──
    if let Some(vocals) = &cfg.vocals16 {
        match audio_context(llm, vocals, &tgt) {
            Ok(ac) if !ac.is_empty() => extra["audio_context"] = Value::from(ac),
            Ok(_) => {}
            Err(e) => log(&format!("  ctx audio skipped: {e}")),
        }
    }

    // ── фаза 4: TRANSLATE весь транскрипт (+тайтлы) С контекстом ───────────
    let n_seg = segs.len();
    let title_texts: Vec<String> = extra["titles"].as_array().map(|a| {
        a.iter().filter_map(|t| t.get("text").and_then(|x| x.as_str()).map(String::from)).collect()
    }).unwrap_or_default();

    let mut lines_all: Vec<String> = segs.iter().enumerate().map(|(i, s)| format!("{}. {}", i + 1, s.text.trim())).collect();
    for (j, t) in title_texts.iter().enumerate() {
        lines_all.push(format!("{}. {}", n_seg + j + 1, t));
    }
    let numbered = lines_all.join("\n");

    let mut ctx = String::new();
    if let Some(sc) = extra["scene_context"].as_str() {
        if !sc.is_empty() {
            ctx += &format!("=== VISUAL SCENE ===\n{sc}\n\n");
        }
    }
    if let Some(ac) = extra["audio_context"].as_str() {
        if !ac.is_empty() {
            ctx += &format!("=== AUDIO (tone/slang/speakers) ===\n{ac}\n\n");
        }
    }

    // TP — ДОСЛОВНО. rewrite -> творческий; иначе точный перевод.
    let tp = if let Some(instr) = rewrite {
        // Творческий ре-дубляж: ЗАМЕНИТЬ содержимое на тему/стиль инструкции, НЕ переводить исходник.
        // «rewrite each line» (как в питоне) наша Q4-QAT читала как «переведи» -> тема не менялась;
        // явное «ignore the source meaning, it's only a rhythm template» заставляет реально переписать (проверено).
        format!(
            "You are a creative scriptwriter writing a BRAND-NEW voice-over script in {tgt} for this short video. \
IGNORE the literal meaning of the source lines — they are ONLY a rhythm/length template. Write a completely NEW \
script whose CONTENT follows this instruction: \"{instr}\". Every line must fit the instruction, NOT translate the \
source. Keep the SAME number of lines and each line about the SAME LENGTH (it will be dubbed to fit the timing). \
Use the scene/audio context below for tone.\n\n{ctx}=== LINES (rhythm template) ===\n{numbered}\n\nOutput ONLY 'N. <line>' per line, nothing else."
        )
    } else {
        format!(
            "Translate EACH numbered line into natural, spoken {tgt} for dubbing — keep the order and the \
numbering, match tone/slang/intent. Use ALL the context below (what the words alone don't convey):\n\n\
{ctx}=== LINES ===\n{numbered}\n\nOutput ONLY 'N. <translation>' per line, nothing else."
        )
    };

    let mt = (80 + 45 * lines_all.len()) as u32;
    let s = Sampling::new(0.2, 0.95, mt).top_k(64);
    let raw = strip_think(&llm.chat(&[Message::user_text(tp)], &s)?);

    // by_n = {N: перевод} из ответа. re.finditer(r"(?m)^\s*(\d+)[.)\]:]\s*(.+?)\s*$")
    let re = Regex::new(r"(?m)^\s*(\d+)[.)\]:]\s*(.+?)\s*$").unwrap();
    let mut by_n: std::collections::HashMap<usize, String> = std::collections::HashMap::new();
    for c in re.captures_iter(&raw) {
        if let Ok(n) = c[1].parse::<usize>() {
            // дубль номера строки: держим ПОСЛЕДНЕЕ вхождение (питон dict-comprehension), НЕ первое (or_insert)
            by_n.insert(n, c[2].trim().to_string());
        }
    }
    for (i, s) in segs.iter_mut().enumerate() {
        let t = by_n.get(&(i + 1)).cloned().unwrap_or_default();
        s.tgt = if t.is_empty() { s.text.trim().to_string() } else { t };
    }
    // переводы тайтлов идут после речевых строк.
    if let Some(arr) = extra["titles"].as_array_mut() {
        for (j, ttl) in arr.iter_mut().enumerate() {
            let default = ttl.get("text").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let tr = by_n.get(&(n_seg + j + 1)).cloned().unwrap_or(default);
            ttl.as_object_mut().map(|o| o.insert("tgt".into(), Value::from(tr)));
        }
    }

    let _ = std::fs::remove_file(&tmp);
    Ok(CtxResult { extra })
}

/// AUDIO-контекст — нарезка вокала на окна <=28с и запрос input_audio. Fail-safe вызывающим.
/// AP — ДОСЛОВНО из ctx_translate.py.
fn audio_context(llm: &ChatClient, vocals: &Path, tgt: &str) -> Result<String, TranslateError> {
    let mut reader = hound::WavReader::open(vocals).map_err(|e| TranslateError::Audio(e.to_string()))?;
    let spec = reader.spec();
    let sr = spec.sample_rate as usize;
    // читаем как f32 mono (микшируем каналы усреднением, как d.mean(axis=1) в питоне)
    let ch = spec.channels as usize;
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().map(|s| s.unwrap_or(0.0)).collect(),
        hound::SampleFormat::Int => {
            let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader.samples::<i32>().map(|s| s.unwrap_or(0) as f32 / max).collect()
        }
    };
    let mono: Vec<f32> = if ch > 1 {
        samples.chunks(ch).map(|c| c.iter().sum::<f32>() / ch as f32).collect()
    } else {
        samples
    };

    let ap = format!(
        "Helping a translator dub to {tgt}. Listen; give context the transcript MISSES (do NOT \
transcribe): situation, tone/register (slang/sarcasm/anger/flirt/...), each speaker gender+vibe, \
slang/idioms and their real meaning here. 4-7 bullets."
    );
    let win = 28 * sr;
    let mut notes: Vec<String> = vec![];
    let mut i = 0;
    while i < mono.len() {
        let end = (i + win).min(mono.len());
        let chunk = &mono[i..end];
        if chunk.len() < 3 * sr {
            i += win;
            continue;
        }
        let wav_b64 = encode_wav_b64(chunk, sr as u32)?;
        let parts = vec![
            Part::AudioB64 { data: wav_b64, format: "wav".into() },
            Part::Text(ap.clone()),
        ];
        let s = Sampling::new(0.2, 0.95, 320).top_k(64);
        let ans = strip_think(&llm.chat(&[Message::user_parts(parts)], &s)?);
        notes.push(format!("[{}s+] {}", i / sr, ans));
        i += win;
    }
    Ok(notes.join("\n\n"))
}

/// Собрать WAV (PCM16) из f32-моно в память и base64-кодировать (как sf.write(buf, ...) в питоне).
fn encode_wav_b64(samples: &[f32], sr: u32) -> Result<String, TranslateError> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: sr,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut buf: Vec<u8> = Vec::new();
    {
        let cursor = std::io::Cursor::new(&mut buf);
        let mut w = hound::WavWriter::new(cursor, spec).map_err(|e| TranslateError::Audio(e.to_string()))?;
        for &x in samples {
            let v = (x.clamp(-1.0, 1.0) * 32767.0) as i16;
            w.write_sample(v).map_err(|e| TranslateError::Audio(e.to_string()))?;
        }
        w.finalize().map_err(|e| TranslateError::Audio(e.to_string()))?;
    }
    Ok(base64::engine::general_purpose::STANDARD.encode(&buf))
}

/// Убрать неиспользуемый импорт-предупреждение (Map используется через json! в некоторых ветках).
#[allow(unused)]
fn _map_marker(_: Map<String, Value>) {}
