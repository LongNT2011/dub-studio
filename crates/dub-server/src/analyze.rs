//! Analyze-ядро (транскрипт-стадия). Порт ASR-части dubengine/pipeline._build_dub + сборки Project
//! из api.analyze/Project.from_artifacts, но БЕЗ перевода/vision/captions (раунд 3).
//!
//! Что покрыто: ffmpeg -> wav 16k mono; диаризация Sortformer (turns); при 1 спикере / пустой
//! диаризации — штатная single-speaker ветка (как в питоне); транскрипция со словными таймстемпами;
//! Project с сегментами (src-текст, words в extra, speaker, mode-дефолты). Поля перевода/капшенов
//! остаются пустыми (tgt_text="", subs.mode как задано, captions по умолчанию).
//!
//! Фазы прогресса (msg + stage) повторяют питон: extract_audio / diarize / asr.

use dub_asr::{Asr, Turn};
use dub_core::{Meta, Project, Segment};
use serde_json::{json, Value};

use crate::media;

/// Параметры analyze (из query POST /projects/{pid}/analyze). tgt_lang/mode/src_lang/subs/rewrite —
/// как в app.py.analyze_project. В этой стадии влияют только на mode-дефолты Project и mode/subs поля.
pub struct AnalyzeArgs {
    pub tgt_lang: String,
    pub mode: String,   // auto | dub | nodub | transcribe (auto -> dub по умолчанию, до vision-стадии)
    pub src_lang: String,
    pub subs: String,   // auto | none | translate | transcribe
    pub rewrite: String,
    pub burn: bool,     // вжигать ли субтитры/титры на видео (композируемость; дефолт true)
}

/// Пути к моделям/входу для одной джобы analyze.
pub struct AnalyzePaths {
    pub input: std::path::PathBuf,   // исходное видео (source.txt)
    pub work_dir: std::path::PathBuf, // workspace/<pid>
    pub tdt_dir: std::path::PathBuf, // каталог TDT-модели
    pub sortformer_onnx: std::path::PathBuf, // sortformer .onnx
    pub llama_bin: std::path::PathBuf, // llama-server(.exe) — сайдкар перевода/vision
    pub mt_model: std::path::PathBuf, // Gemma GGUF
    pub mmproj: std::path::PathBuf,   // mmproj GGUF (vision-проектор)
    pub models_root: std::path::PathBuf, // корень моделей (для OCR: <root>/ocr/…)
    pub caption_fps: i32,             // частота семплинга кадров OCR
}

/// Колбэк прогресса джобы (msg + произвольные поля). stage — фаза как в питоне.
pub type Progress<'a> = dyn Fn(Value) + Send + Sync + 'a;

fn emit(progress: &Progress, stage: &str, msg: &str) {
    progress(json!({ "stage": stage, "msg": msg }));
}

/// _has_speech (порт pipeline.py:66-78): есть ли РЕАЛЬНАЯ дубляж-годная речь? ASR галлюцинирует на
/// музыке/неподдерживаемом языке — обычно короткая фраза повторяется (заполняет таймлайн, но почти без
/// РАЗНООБРАЗИЯ слов). Гейтим по разнообразию слов + покрытию: uniq>=0.35 и coverage>=0.10. Пустой
/// транскрипт (<4 слов) -> нет речи.
fn has_speech(segs: &[Segment], total: f64) -> bool {
    let mut words: Vec<String> = Vec::new();
    for s in segs {
        for w in s.src_text.split(|c: char| !c.is_alphabetic()) {
            if !w.is_empty() {
                words.push(w.to_lowercase());
            }
        }
    }
    if words.len() < 4 {
        return false;
    }
    let cov: f64 = segs.iter().map(|s| (s.end - s.start).max(0.0)).sum::<f64>() / total.max(0.1);
    let uniq = words.iter().collect::<std::collections::HashSet<_>>().len() as f64 / words.len() as f64;
    cov >= 0.10 && uniq >= 0.35
}

/// Разбить mode/subs как в pipeline: dub vs subs-only vs transcribe. В транскрипт-стадии нам нужно лишь
/// решить итоговый Project.mode и Project.subs.mode. auto -> dub (флип в nodub делает has_speech-гейт
/// ниже, после ASR — как в питоне pipeline.py:124-129).
fn resolve_modes(args: &AnalyzeArgs) -> (String, String) {
    // subs=auto: для dub/transcribe режимов оставляем translate (перевод придёт в раунде 3), для
    // transcribe-режима — transcribe. Совпадает с логикой api.analyze mode-присвоения.
    let mode = match args.mode.as_str() {
        "nodub" => "nodub",
        "transcribe" => "transcribe",
        "voiceover" => "voiceover", // закадровый: переводим+TTS, но оригинал слышно приглушённым
        _ => "dub", // dub | auto -> dub
    }
    .to_string();
    let subs = match (args.subs.as_str(), mode.as_str()) {
        ("none", _) => "none",
        ("transcribe", _) | (_, "transcribe") => "transcribe",
        // dub / nodub c subs=auto|translate -> translate
        _ => "translate",
    }
    .to_string();
    (mode, subs)
}

/// Запустить analyze: extract -> diarize -> transcribe -> Project. Возвращает готовый Project.
/// Диаризация/транскрипция тяжёлые (ONNX CPU) — вызывать в блокирующем контексте (job worker).
pub fn run(args: &AnalyzeArgs, paths: &AnalyzePaths, progress: &Progress) -> Result<Project, String> {
    // 1) probe (метаданные видео).
    let meta = media::probe(&paths.input)?;
    emit(
        progress,
        "probe",
        &format!(
            "input {} dur={:.1}s {}x{}",
            paths.input.file_name().and_then(|s| s.to_str()).unwrap_or("?"),
            meta.duration,
            meta.width,
            meta.height
        ),
    );

    // 2) extract audio -> wav 16k mono.
    emit(progress, "extract_audio", "извлечение аудио (ffmpeg -> 16k mono)");
    let vocals16 = paths.work_dir.join("vocals16.wav");
    media::extract_wav_16k_mono(&paths.input, &vocals16)?;

    // 3) диаризация. merge_gap=0.8, min_speaker_dur=2.5 — дефолты diarize.turns питона (asr.py-контракт).
    //    Если модель sortformer недоступна ИЛИ вернула <2 спикеров -> single-speaker (штатная ветка).
    //    ГЕЙТ. Два случая: (1) dub/auto — ПАРИТЕТ с питоном (pipeline.py:169 `cfg.dub and _per_spk`; голос по
    //    умолчанию clone -> _per_spk=true, поэтому диаризуем). (2) transcribe — НАМЕРЕННОЕ РАСШИРЕНИЕ порта:
    //    в питоне transcribe ставит cfg.dub=False и НЕ диаризует, но у нас это отдельный режим «транскрипт+
    //    диаризация» (спикеры показываются в UI), поэтому диаризуем. nodub (только субтитры) — НЕ диаризуем
    //    (как питон whole-clip; иначе diarize-first порезал бы субтитры по спикерам иначе, чем оригинал).
    let want_diar = args.mode != "nodub";
    emit(progress, "diarize", "диаризация (Sortformer)");
    let diar = if want_diar && paths.sortformer_onnx.is_file() {
        match dub_asr::turns(&vocals16, &paths.sortformer_onnx, 0.8, 2.5) {
            Ok(d) => Some(d),
            Err(e) => {
                emit(
                    progress,
                    "diarize",
                    &format!("диаризация не удалась ({e}); single-speaker путь"),
                );
                None
            }
        }
    } else {
        emit(
            progress,
            "diarize",
            if !want_diar { "субтитры: без диаризации (whole-clip, как питон)" } else { "sortformer-модель не найдена; single-speaker путь" },
        );
        None
    };

    // 4) транскрипция. n_spk>1 -> transcribe_turns (каждая реплика одного спикера); иначе whole-clip.
    let mut asr = Asr::new(&paths.tdt_dir);
    let (segments, n_spk): (Vec<Segment>, usize) = match &diar {
        Some(d) if d.n_speakers > 1 && !d.turns.is_empty() => {
            emit(
                progress,
                "asr",
                &format!("транскрипция по репликам ({} спикеров)", d.n_speakers),
            );
            let turns: Vec<Turn> = d.turns.clone();
            let mut sp = asr
                .transcribe_turns(&vocals16, &turns)
                .map_err(|e| format!("transcribe_turns: {e}"))?;
            // diarize-first складывает реплики ПО СПИКЕРАМ, не по времени -> сортируем по start
            // (как segs.sort в питоне), чтобы порядок сегментов был временной.
            sp.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal));
            let segs = sp
                .into_iter()
                .enumerate()
                .map(|(i, s)| Segment {
                    id: format!("s{i}"),
                    start: s.start,
                    end: s.end,
                    speaker: Some(s.speaker.to_string()),
                    src_text: s.text,
                    tgt_text: String::new(),
                    voice: None,
                    dirty: false,
                    extra: Default::default(),
                })
                .collect();
            (segs, d.n_speakers)
        }
        _ => {
            emit(progress, "asr", "транскрипция всего клипа (single-speaker)");
            let ts = asr
                .transcribe(&vocals16, &args.src_lang)
                .map_err(|e| format!("transcribe: {e}"))?;
            let segs = ts
                .into_iter()
                .enumerate()
                .map(|(i, s)| {
                    // words сохраняем в extra (контракт python transcript кладёт words в сегмент;
                    // dub-core Segment не типизирует words, но extra="allow" их проносит).
                    let words: Vec<Value> = s
                        .words
                        .iter()
                        .map(|w| json!({ "word": w.word, "start": w.start, "end": w.end }))
                        .collect();
                    let mut extra = serde_json::Map::new();
                    extra.insert("words".into(), Value::Array(words));
                    Segment {
                        id: format!("s{i}"),
                        start: s.start,
                        end: s.end,
                        // питон pipeline.run пишет speaker=s.get("speaker",0)=0 для single-speaker,
                        // и from_artifacts кладёт str(0)="0" -> держим тот же контракт.
                        speaker: Some("0".to_string()),
                        src_text: s.text,
                        tgt_text: String::new(),
                        voice: None,
                        dirty: false,
                        extra,
                    }
                })
                .collect();
            (segs, 1)
        }
    };

    emit(
        progress,
        "asr",
        &format!("{} сегментов, {} спикер(ов)", segments.len(), n_spk),
    );

    // 5) собрать Project по контракту dub-core (mode-дефолты).
    let (mut mode, subs_mode) = resolve_modes(args);
    // auto -> nodub гейт (порт pipeline.py:124-129): режим auto дублирует ТОЛЬКО при реальной речи;
    // музыкальный/иноязычный клип (пустой транскрипт ИЛИ галлюцинация-повтор) -> nodub (оставить
    // оригинальную дорожку, локализовать лишь экранный текст). Пустые сегменты -> тоже nodub.
    if args.mode == "auto" && (segments.is_empty() || !has_speech(&segments, meta.duration)) {
        mode = "nodub".to_string();
        emit(
            progress,
            "asr",
            if segments.is_empty() {
                "нет речевых сегментов; оставляю оригинальную дорожку (nodub)"
            } else {
                "auto: нет дубляж-годной речи -> NODUB (оригинал + локализация экранного текста)"
            },
        );
    }
    let mut proj = Project::default();
    proj.meta = Meta {
        video: paths.input.to_string_lossy().into_owned(),
        duration: meta.duration,
        width: meta.width,
        height: meta.height,
        fps: meta.fps,
        src_codec: meta.src_codec,
        extra: Default::default(),
    };
    proj.mode = mode;
    proj.tgt_lang = args.tgt_lang.clone();
    proj.subs.mode = subs_mode;
    proj.subs.burn = args.burn; // композируемость: вжигать субтитры/титры или нет
    proj.segments = segments;
    proj.work_dir = Some(paths.work_dir.to_string_lossy().into_owned());
    if !args.rewrite.is_empty() {
        proj.audio.rewrite = Some(args.rewrite.clone());
    }

    // 6) стадии translate + vision (раунд 3). Порт pipeline._build_dub translate-ветки: если есть
    //    что переводить, поднимаем llama-server (сайдкар Gemma+mmproj), гоним единый ctx-проход
    //    (vision layout/scene + перевод всего транскрипта), заполняем tgt_text/titles/sub_style.
    //    Пайплайн последовательный: TTS в этот момент не загружен (как tts.release() в питоне) —
    //    Gemma получает всю VRAM. Fail-safe: сбой стадии оставляет tgt пустым (перевод — не блокер analyze).
    let vocals16 = paths.work_dir.join("vocals16.wav");
    crate::translate::stage(args, paths, &mut proj, &vocals16, meta.height, meta.duration, progress);

    // 7) OCR-стадия (раунд 4): детекция вшитого текста -> блюр-боксы субтитр-полосы + уточнение sub_y.
    //    Порт pipeline.run ocr_detect + compose.analyze_layout. Fail-safe: сбой OCR не валит analyze
    //    (боксы блюра — не блокер; их можно добавить руками в редакторе).
    crate::ocr::stage(args, paths, &mut proj, meta.width, meta.height, meta.duration, progress);

    Ok(proj)
}
