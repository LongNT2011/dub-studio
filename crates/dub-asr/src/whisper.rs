//! Whisper-движок ASR как альтернатива Parakeet: обёртка над standalone-бинарём Purfview
//! (whisper-standalone-win, faster-whisper на CTranslate2). Запускаем `whisper-faster.exe` сабпроцессом,
//! просим `--output_format json --word_timestamps True`, парсим словные таймстемпы -> те же Word/Segment,
//! что у Parakeet (единый контракт сегментации `segment_words`). Так пользователь может выбрать движок
//! ASR (Parakeet/Whisper), РАЗНЫЕ модели (tiny…large-v3/turbo) и РАЗНЫЕ кванты (compute_type).
//!
//! Модель ищется локально (`--model <size> --model_dir <dir>`, каталог `faster-whisper-<size>`), сеть
//! глушим `HF_HUB_OFFLINE=1` (оффлайн-first). Диаризация остаётся на Sortformer (как у Parakeet): для
//! per-speaker раскладываем слова whole-clip по репликам, затем сегментируем внутри каждой.

use crate::segment::{segment_words, Segment, Word, SEG_MAX_GAP, SEG_MAX_DUR};
use crate::{AsrEngine, AsrError, SpeakerSegment, Turn};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Движок Whisper (standalone faster-whisper). Держит параметры запуска; модель грузится сабпроцессом
/// на каждый вызов (бинарь — отдельный процесс, состояние между вызовами не держим).
pub struct WhisperAsr {
    /// Путь к whisper-faster.exe.
    bin: PathBuf,
    /// Каталог с моделями (внутри — `faster-whisper-<size>`).
    model_dir: PathBuf,
    /// Имя модели: tiny|base|small|medium|large-v3|large-v3-turbo.
    model: String,
    /// compute_type (квант): int8|int8_float32|float32|float16|…
    compute: String,
    /// Устройство исполнения: cpu|cuda.
    device: String,
}

impl WhisperAsr {
    pub fn new(
        bin: impl AsRef<Path>,
        model_dir: impl AsRef<Path>,
        model: impl Into<String>,
        compute: impl Into<String>,
        device: impl Into<String>,
    ) -> Self {
        Self {
            bin: bin.as_ref().to_path_buf(),
            model_dir: model_dir.as_ref().to_path_buf(),
            model: model.into(),
            compute: compute.into(),
            device: device.into(),
        }
    }

    /// Прогнать whisper-faster.exe на WAV, вернуть словный поток (абсолютные секунды). lang="auto" ->
    /// авто-детект (флаг --language не передаём). Оффлайн: HF_HUB_OFFLINE=1, модель из --model_dir.
    fn run_words(&self, wav: &Path, lang: &str) -> Result<Vec<Word>, AsrError> {
        let cwd = std::env::current_dir().ok();
        let abs = |p: &Path| -> PathBuf {
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                match &cwd {
                    Some(c) => c.join(p),
                    None => p.to_path_buf(),
                }
            }
        };
        // Свежий выходной каталог рядом с WAV — читаем единственный *.json, старьё не мешает.
        let stem = wav.file_stem().and_then(|s| s.to_str()).unwrap_or("audio");
        let out_dir = abs(wav.parent().unwrap_or(Path::new("."))).join(format!("wsp_{stem}"));
        let _ = std::fs::remove_dir_all(&out_dir);
        std::fs::create_dir_all(&out_dir).map_err(|e| AsrError::Io(e.to_string()))?;

        let mut cmd = Command::new(abs(&self.bin));
        cmd.arg(abs(wav))
            .arg("--model").arg(&self.model)
            .arg("--model_dir").arg(abs(&self.model_dir))
            .arg("--task").arg("transcribe")
            .arg("--output_format").arg("json")
            .arg("--output_dir").arg(&out_dir)
            .arg("--compute_type").arg(&self.compute)
            .arg("--device").arg(&self.device)
            .arg("--word_timestamps").arg("True")
            .arg("--beep_off");
        // Язык: конкретный код -> фиксируем (быстрее и точнее), "auto"/пусто -> авто-детект.
        let l = lang.trim();
        if !l.is_empty() && l != "auto" {
            cmd.arg("--language").arg(l);
        }
        // Оффлайн: не ходить в сеть за моделью (веса уже на диске).
        cmd.env("HF_HUB_OFFLINE", "1").env("TRANSFORMERS_OFFLINE", "1");
        if let Some(dir) = self.bin.parent() {
            cmd.current_dir(abs(dir)); // ради bundled CTranslate2/oneDNN-DLL рядом с бинарём
        }
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }
        let out = cmd.output().map_err(|e| AsrError::Parakeet(format!("whisper spawn: {e}")))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let stdout = String::from_utf8_lossy(&out.stdout);
            let mut tail: Vec<&str> = stderr.lines().chain(stdout.lines()).rev().take(10).collect();
            tail.reverse();
            return Err(AsrError::Parakeet(format!(
                "whisper код {:?}: {}",
                out.status.code(),
                tail.join(" | ")
            )));
        }

        // Читаем единственный *.json из out_dir.
        let json_path = std::fs::read_dir(&out_dir)
            .map_err(|e| AsrError::Io(e.to_string()))?
            .flatten()
            .map(|e| e.path())
            .find(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
            .ok_or_else(|| AsrError::Parakeet("whisper: нет JSON-вывода".into()))?;
        let txt = std::fs::read_to_string(&json_path)
            .map_err(|e| AsrError::WavRead(json_path.display().to_string(), e.to_string()))?;
        let words = parse_whisper_json(&txt);
        let _ = std::fs::remove_dir_all(&out_dir);
        Ok(words)
    }
}

impl AsrEngine for WhisperAsr {
    fn transcribe(&mut self, wav: &Path, lang: &str) -> Result<Vec<Segment>, AsrError> {
        let words = self.run_words(wav, lang)?;
        Ok(segment_words(&words, SEG_MAX_GAP, SEG_MAX_DUR))
    }

    /// Per-speaker: whole-clip ОДИН прогон (сабпроцесс дорогой на старт), затем слова раскладываем по
    /// репликам (по середине слова во временном окне реплики; слово вне всех окон -> ближайшая реплика),
    /// внутри каждой — обычная сегментация. Времена уже абсолютные.
    fn transcribe_turns(&mut self, wav: &Path, turns: &[Turn], lang: &str) -> Result<Vec<SpeakerSegment>, AsrError> {
        let words = self.run_words(wav, lang)?;
        if turns.is_empty() {
            // нет реплик -> single-speaker (0), как fallback
            return Ok(segment_words(&words, SEG_MAX_GAP, SEG_MAX_DUR)
                .into_iter()
                .map(|s| SpeakerSegment { start: s.start, end: s.end, text: s.text, speaker: 0 })
                .collect());
        }
        // Ведро слов на реплику.
        let mut buckets: Vec<Vec<Word>> = vec![Vec::new(); turns.len()];
        for w in words {
            let mid = (w.start + w.end) / 2.0;
            // реплика, чьё окно содержит середину; иначе — ближайшая по центру окна.
            let idx = turns
                .iter()
                .position(|t| mid >= t.start && mid <= t.end)
                .unwrap_or_else(|| {
                    turns
                        .iter()
                        .enumerate()
                        .min_by(|(_, a), (_, b)| {
                            let da = (mid - (a.start + a.end) / 2.0).abs();
                            let db = (mid - (b.start + b.end) / 2.0).abs();
                            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                        })
                        .map(|(i, _)| i)
                        .unwrap_or(0)
                });
            buckets[idx].push(w);
        }
        let mut out = Vec::new();
        for (i, ws) in buckets.into_iter().enumerate() {
            if ws.is_empty() {
                continue;
            }
            let spk = turns[i].speaker;
            for s in segment_words(&ws, SEG_MAX_GAP, SEG_MAX_DUR) {
                out.push(SpeakerSegment { start: s.start, end: s.end, text: s.text, speaker: spk });
            }
        }
        out.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal));
        Ok(out)
    }
}

/// Распарсить JSON faster-whisper (openai-формат) в словный поток. Берём words каждого сегмента
/// ({word,start,end}); если у сегмента нет words — сам сегмент как одно «слово» (fallback). Ведущий
/// пробел в word тримим (segment_words соединяет через " "). Устойчиво к отсутствующим полям.
fn parse_whisper_json(txt: &str) -> Vec<Word> {
    let v: serde_json::Value = match serde_json::from_str(txt) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let mut words = Vec::new();
    let Some(segs) = v.get("segments").and_then(|s| s.as_array()) else {
        return words;
    };
    for seg in segs {
        let ws = seg.get("words").and_then(|w| w.as_array());
        match ws {
            Some(arr) if !arr.is_empty() => {
                for w in arr {
                    let word = w
                        .get("word")
                        .or_else(|| w.get("text"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    if word.is_empty() {
                        continue;
                    }
                    let start = w.get("start").and_then(|x| x.as_f64()).unwrap_or(0.0);
                    let end = w.get("end").and_then(|x| x.as_f64()).unwrap_or(start);
                    words.push(Word { word, start, end: end.max(start) });
                }
            }
            _ => {
                // сегмент без словных таймстемпов — одно «слово» на весь сегмент
                let word = seg.get("text").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
                if !word.is_empty() {
                    let start = seg.get("start").and_then(|x| x.as_f64()).unwrap_or(0.0);
                    let end = seg.get("end").and_then(|x| x.as_f64()).unwrap_or(start);
                    words.push(Word { word, start, end: end.max(start) });
                }
            }
        }
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_word_timestamps() {
        let j = r#"{"segments":[
            {"start":0.0,"end":0.8,"text":" Hello world.","words":[
                {"word":" Hello","start":0.0,"end":0.4,"probability":0.9},
                {"word":" world.","start":0.4,"end":0.8,"probability":0.8}]}],
            "language":"en"}"#;
        let ws = parse_whisper_json(j);
        assert_eq!(ws.len(), 2);
        assert_eq!(ws[0].word, "Hello");
        assert_eq!(ws[1].word, "world.");
        assert!((ws[1].end - 0.8).abs() < 1e-6);
    }

    #[test]
    fn falls_back_to_segment_when_no_words() {
        let j = r#"{"segments":[{"start":1.0,"end":2.0,"text":" No words here"}],"language":"ru"}"#;
        let ws = parse_whisper_json(j);
        assert_eq!(ws.len(), 1);
        assert_eq!(ws[0].word, "No words here");
    }

    #[test]
    fn empty_on_garbage() {
        assert!(parse_whisper_json("not json").is_empty());
        assert!(parse_whisper_json("{}").is_empty());
    }
}
