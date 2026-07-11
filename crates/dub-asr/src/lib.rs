//! dub-asr — ASR со словными таймстемпами + диаризация поверх parakeet-rs.
//!
//! Движок: Parakeet-TDT-0.6B-v3 (мультиязычный, авто-определение языка) + Sortformer v2 для диаризации,
//! оба через ONNX Runtime (провайдер CPU по умолчанию). parakeet-rs требует ровно 16 кГц моно —
//! входной WAV приводится к 16k/mono здесь (даунмикс + линейный ресемпл).
//!
//! Сегментация словного потока (_segment), transcribe / diarize / transcribe_turns — порт
//! dubengine/asr.py и dubengine/diarize.py: паузы >0.6с, конец предложения .!?…, макс 8.0с.

mod segment;

use parakeet_rs::sortformer::{DiarizationConfig, Sortformer};
use parakeet_rs::{ExecutionConfig, ParakeetTDT, TimestampMode, Transcriber};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Конфиг исполнения ONNX. КРИТИЧНО: понижаем уровень оптимизации графа до Level1 — на int8-кванте
/// Parakeet дефолтный Level3 виснет при создании CPU-сессии на минуты (оптимайзер спинит на
/// DynamicQuantizeLinear/MatMulInteger). Переопределяется через DUB_ASR_OPT_LEVEL (0..3).
fn exec_config() -> ExecutionConfig {
    use ort::session::builder::GraphOptimizationLevel;
    let level = std::env::var("DUB_ASR_OPT_LEVEL")
        .ok()
        .and_then(|s| s.trim().parse::<u8>().ok())
        .unwrap_or(1);
    ExecutionConfig::new().with_custom_configure(move |b| {
        let lvl = match level {
            0 => GraphOptimizationLevel::Disable,
            2 => GraphOptimizationLevel::Level2,
            3 => GraphOptimizationLevel::Level3,
            _ => GraphOptimizationLevel::Level1,
        };
        Ok(b.with_optimization_level(lvl)?)
    })
}

pub use segment::{segment_words, Segment, Word};

/// Целевая частота parakeet-rs.
pub const TARGET_SR: u32 = 16_000;

#[derive(Error, Debug)]
pub enum AsrError {
    #[error("parakeet: {0}")]
    Parakeet(String),
    #[error("не удалось прочитать wav {0}: {1}")]
    WavRead(String, String),
    #[error("io: {0}")]
    Io(String),
}

/// Одна реплика диаризации: [start, end] в секундах, speaker — контиг. id (0..k-1).
#[derive(Debug, Clone, serde::Serialize)]
pub struct Turn {
    pub start: f64,
    pub end: f64,
    pub speaker: i32,
}

/// Сегмент с привязкой к спикеру (результат transcribe_turns).
#[derive(Debug, Clone, serde::Serialize)]
pub struct SpeakerSegment {
    pub start: f64,
    pub end: f64,
    pub text: String,
    pub speaker: i32,
}

/// ASR-движок: держит загруженную TDT-модель тёплой между вызовами.
pub struct Asr {
    tdt_dir: PathBuf,
    model: Option<ParakeetTDT>,
}

impl Asr {
    /// Каталог с TDT-моделью (encoder-model.onnx + .data, decoder_joint-model.onnx, vocab.txt).
    pub fn new(tdt_dir: impl AsRef<Path>) -> Self {
        Self {
            tdt_dir: tdt_dir.as_ref().to_path_buf(),
            model: None,
        }
    }

    fn model(&mut self) -> Result<&mut ParakeetTDT, AsrError> {
        if self.model.is_none() {
            let m = ParakeetTDT::from_pretrained(&self.tdt_dir, Some(exec_config()))
                .map_err(|e| AsrError::Parakeet(e.to_string()))?;
            self.model = Some(m);
        }
        Ok(self.model.as_mut().unwrap())
    }

    /// Транскрипция всего клипа со словными таймстемпами -> сегменты по паузам/пунктуации/макс-длине.
    /// Порт asr.transcribe: словный поток -> _segment. `_lang` зарезервирован (TDT сам определяет язык).
    pub fn transcribe(&mut self, wav: impl AsRef<Path>, _lang: &str) -> Result<Vec<Segment>, AsrError> {
        let (audio, sr) = load_wav_16k_mono(wav.as_ref())?;
        let words = self.transcribe_words(&audio, sr)?;
        Ok(segment_words(&words, 0.6, 8.0))
    }

    /// Прогнать модель на семплах 16k/mono и получить словные таймстемпы (TimestampMode::Words).
    fn transcribe_words(&mut self, audio: &[f32], sr: u32) -> Result<Vec<Word>, AsrError> {
        let model = self.model()?;
        let res = model
            .transcribe_samples(audio.to_vec(), sr, 1, Some(TimestampMode::Words))
            .map_err(|e| AsrError::Parakeet(e.to_string()))?;
        // parakeet-rs в режиме Words отдаёт .tokens уже как слова (text/start/end в секундах).
        let audio_end = audio.len() as f64 / sr as f64;
        Ok(res
            .tokens
            .into_iter()
            .filter(|t| !t.text.trim().is_empty())
            .map(|t| Word {
                word: t.text.trim().to_string(),
                start: t.start as f64,
                end: (t.end as f64).max(t.start as f64),
            })
            .map(|mut w| {
                if w.end <= w.start {
                    w.end = audio_end.max(w.start);
                }
                w
            })
            .collect())
    }

    /// DIARIZE-FIRST: транскрибировать КАЖДУЮ реплику отдельно (один спикер на сегмент). Порт
    /// asr.transcribe_turns: клип на turn, транскрипция, паузная разбивка внутри turn.
    pub fn transcribe_turns(
        &mut self,
        wav: impl AsRef<Path>,
        turns: &[Turn],
    ) -> Result<Vec<SpeakerSegment>, AsrError> {
        let (audio, sr) = load_wav_16k_mono(wav.as_ref())?;
        let min_len = (0.2 * sr as f64) as usize;
        let mut out = Vec::new();
        for t in turns {
            let a = (t.start * sr as f64) as usize;
            let b = ((t.end * sr as f64) as usize).min(audio.len());
            if b <= a || (b - a) < min_len {
                continue; // слишком коротко для транскрипции
            }
            let clip = &audio[a..b];
            let words = self.transcribe_words(clip, sr)?;
            // Паузная разбивка ВНУТРИ реплики, чтобы длинный монолог не стал одним гигантским сегментом.
            for s in segment_words(&words, 0.6, 8.0) {
                out.push(SpeakerSegment {
                    start: t.start + s.start,
                    end: t.start + s.end,
                    text: s.text,
                    speaker: t.speaker,
                });
            }
        }
        Ok(out)
    }
}

/// Диаризация: Sortformer v2 -> реплики [(start,end,speaker)] в секундах, speaker перенумерован 0..k-1.
/// sortformer_onnx — путь к diar_streaming_sortformer_4spk-v2.onnx.
pub fn diarize(
    wav: impl AsRef<Path>,
    sortformer_onnx: impl AsRef<Path>,
) -> Result<Vec<Turn>, AsrError> {
    let (audio, sr) = load_wav_16k_mono(wav.as_ref())?;
    let mut sf = Sortformer::with_config(
        sortformer_onnx.as_ref(),
        Some(exec_config()),
        DiarizationConfig::callhome(),
    )
    .map_err(|e| AsrError::Parakeet(e.to_string()))?;
    let segs = sf
        .diarize(audio, sr, 1)
        .map_err(|e| AsrError::Parakeet(e.to_string()))?;
    // Sortformer отдаёт start/end в СЕМПЛАХ (при 16 кГц). Перенумеруем спикеров в контиг. 0..k-1.
    let mut raw: Vec<Turn> = segs
        .iter()
        .map(|s| Turn {
            start: s.start as f64 / TARGET_SR as f64,
            end: s.end as f64 / TARGET_SR as f64,
            speaker: s.speaker_id as i32,
        })
        .collect();
    raw.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal));
    let mut labels: Vec<i32> = raw.iter().map(|t| t.speaker).collect();
    labels.sort_unstable();
    labels.dedup();
    for t in &mut raw {
        t.speaker = labels.iter().position(|&l| l == t.speaker).unwrap_or(0) as i32;
    }
    Ok(raw)
}

/// Окно референса спикера: [start, end] его самой длинной реплики (для клон-x-вектора).
pub type RefWindow = (f64, f64);

/// Результат turns(): (реплики, число спикеров, ref_windows: speaker -> самая длинная реплика).
pub struct DiarTurns {
    pub turns: Vec<Turn>,
    pub n_speakers: usize,
    pub ref_windows: std::collections::HashMap<i32, RefWindow>,
}

/// DIARIZE-FIRST: порт diarize.turns() — слить подряд идущие реплики одного спикера (merge_gap),
/// и если «настоящих» спикеров (суммарно >= min_speaker_dur) меньше двух, схлопнуть в single-speaker
/// (turns=[], n=1) — это ШТАТНАЯ graceful-деградация питона, не отсебятина. Иначе перенумеровать
/// спикеров 0..k-1 и вернуть ref_windows (самая длинная реплика каждого).
pub fn turns(
    wav: impl AsRef<Path>,
    sortformer_onnx: impl AsRef<Path>,
    merge_gap: f64,
    min_speaker_dur: f64,
) -> Result<DiarTurns, AsrError> {
    use std::collections::HashMap;
    let single = |_| DiarTurns { turns: Vec::new(), n_speakers: 1, ref_windows: HashMap::new() };

    let raw = diarize(wav, sortformer_onnx)?;
    if raw.is_empty() {
        return Ok(single(()));
    }

    // Слить подряд идущие реплики одного спикера с зазором <= merge_gap.
    let mut merged: Vec<[f64; 3]> = vec![[raw[0].start, raw[0].end, raw[0].speaker as f64]];
    for t in &raw[1..] {
        let last = merged.last_mut().unwrap();
        if t.speaker as f64 == last[2] && t.start - last[1] <= merge_gap {
            last[1] = last[1].max(t.end);
        } else {
            merged.push([t.start, t.end, t.speaker as f64]);
        }
    }

    // Суммарная длительность на спикера -> «настоящие» спикеры (>= min_speaker_dur).
    let mut dur: HashMap<i32, f64> = HashMap::new();
    for m in &merged {
        *dur.entry(m[2] as i32).or_insert(0.0) += m[1] - m[0];
    }
    let real: Vec<i32> = dur.iter().filter(|(_, &d)| d >= min_speaker_dur).map(|(&s, _)| s).collect();
    if real.len() < 2 {
        return Ok(single(())); // реально один голос -> single-speaker путь
    }
    let realset: std::collections::HashSet<i32> = real.iter().copied().collect();

    // Крошечную реплику не-настоящего спикера переназначить ближайшей настоящей (по середине).
    let real_turns: Vec<[f64; 3]> = merged.iter().filter(|m| realset.contains(&(m[2] as i32))).copied().collect();
    for m in &mut merged {
        if !realset.contains(&(m[2] as i32)) {
            let mid = (m[0] + m[1]) / 2.0;
            let nearest = real_turns
                .iter()
                .min_by(|a, b| {
                    let da = (mid - (a[0] + a[1]) / 2.0).abs();
                    let db = (mid - (b[0] + b[1]) / 2.0).abs();
                    da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|x| x[2])
                .unwrap_or(m[2]);
            m[2] = nearest;
        }
    }

    // Самая длинная реплика каждого спикера (до перенумерации).
    let mut longest: HashMap<i32, RefWindow> = HashMap::new();
    for m in &merged {
        let sp = m[2] as i32;
        let cur = longest.get(&sp).copied().unwrap_or((0.0, 0.0));
        if (m[1] - m[0]) > (cur.1 - cur.0) {
            longest.insert(sp, (m[0], m[1]));
        }
    }

    // Перенумеровать метки в 0..k-1 по возрастанию.
    let mut labels: Vec<i32> = merged.iter().map(|m| m[2] as i32).collect();
    labels.sort_unstable();
    labels.dedup();
    let remap: HashMap<i32, i32> = labels.iter().enumerate().map(|(i, &l)| (l, i as i32)).collect();

    let out: Vec<Turn> = merged
        .iter()
        .map(|m| Turn { start: m[0], end: m[1], speaker: remap[&(m[2] as i32)] })
        .collect();
    let rw: HashMap<i32, RefWindow> = longest.into_iter().map(|(old, w)| (remap[&old], w)).collect();
    Ok(DiarTurns { turns: out, n_speakers: labels.len(), ref_windows: rw })
}

// ─── загрузка/подготовка аудио ──────────────────────────────────────────────

/// Прочитать WAV, свести в моно и ресемплировать в 16 кГц (parakeet-rs требует ровно 16k моно).
fn load_wav_16k_mono(path: &Path) -> Result<(Vec<f32>, u32), AsrError> {
    let mut reader = hound::WavReader::open(path)
        .map_err(|e| AsrError::WavRead(path.display().to_string(), e.to_string()))?;
    let spec = reader.spec();
    let ch = spec.channels.max(1) as usize;

    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AsrError::WavRead(path.display().to_string(), e.to_string()))?,
        hound::SampleFormat::Int => {
            let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / max))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| AsrError::WavRead(path.display().to_string(), e.to_string()))?
        }
    };

    // Даунмикс в моно.
    let mono: Vec<f32> = if ch <= 1 {
        interleaved
    } else {
        interleaved
            .chunks(ch)
            .map(|frame| frame.iter().sum::<f32>() / ch as f32)
            .collect()
    };

    let out = if spec.sample_rate == TARGET_SR {
        mono
    } else {
        resample_linear(&mono, spec.sample_rate, TARGET_SR)
    };
    Ok((out, TARGET_SR))
}

/// Линейный ресемпл. Для извлечения мел-фич ASR этого достаточно; тяжёлый sinc не нужен.
fn resample_linear(input: &[f32], src_sr: u32, dst_sr: u32) -> Vec<f32> {
    if input.is_empty() || src_sr == dst_sr {
        return input.to_vec();
    }
    let ratio = dst_sr as f64 / src_sr as f64;
    let out_len = ((input.len() as f64) * ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src_pos = i as f64 / ratio;
        let idx = src_pos.floor() as usize;
        let frac = (src_pos - idx as f64) as f32;
        let a = input.get(idx).copied().unwrap_or(0.0);
        let b = input.get(idx + 1).copied().unwrap_or(a);
        out.push(a + (b - a) * frac);
    }
    out
}
