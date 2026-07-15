//! Рендер-ядро (порт render-половины pipeline.run + _build_dub TTS-ветки + assemble + compose/mix +
//! captions.build/burn + mux). От Project с переводом до готового дублированного MP4.
//!
//! Конвейер (последовательный, GPU-стадии по очереди — VRAM-инвариант):
//!   probe -> extract 44.1k -> separate (vocals/instrumental через dub-sep) ->
//!   per-segment Higgs clone TTS (реф из вокала) -> fit_to_slot (atempo) -> timeline ->
//!   mix (instrumental + dub) -> build ASS (титры/субтитры/sub_style из Project) ->
//!   burn (blur боксы из project.captions.blur_boxes) -> mux.
//!
//! regen (на экспорте): ре-TTS ТОЛЬКО dirty-сегментов. Кэш per-segment WAV в work_dir (seg_XXX.wav):
//! не-dirty переиспользуются, dirty пере-синтезируются (улучшение против питон-_regen_dub, что гнал
//! весь дубляж заново — правка #10). tgt-текст пустой -> сегмент молчит (как в питоне).

use audiocpp::AudiocppEngine;
use dub_captions::{BlurBox, Sub, SubStyle as CapSubStyle, Title as CapTitle};
use dub_core::{Project, SubStyle as CoreSubStyle, Title as CoreTitle};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::media;
use crate::wavio;

/// Пути к моделям/движкам для рендера (резолвятся из AppState).
pub struct RenderPaths {
    pub input: PathBuf,        // исходное видео
    pub work_dir: PathBuf,     // workspace/<pid>
    pub output: PathBuf,       // output.mp4
    pub bsroformer_cli: PathBuf,
    pub bsroformer_model: PathBuf,
    pub higgs_dll: PathBuf,
    pub higgs_model_root: PathBuf,
    pub higgs_quant: String,   // квант выбранного варианта Higgs (q8_0/q6_k/q4_k_m) — для audiocpp load_model
    pub fonts_dir: PathBuf,
    pub higgs_backend: String, // "cuda" | "cpu"
    pub higgs_device: i32,
    pub higgs_threads: i32,
    pub max_stretch: f64,
    pub voices_dir: PathBuf,   // каталог голосов-паков + записей с микрофона
    pub asr: crate::models::AsrChoice, // выбранный ASR-движок — авто-транскрипция реф-клипа (ref_text клона)
    pub ref_secs: f64,         // длина реф-клипа клона голоса, сек (настройка «Экономия RAM», дефолт 12.0)
}

pub type Progress<'a> = dyn Fn(Value) + Send + Sync + 'a;

fn emit(progress: &Progress, stage: &str, msg: &str) {
    progress(json!({ "stage": stage, "msg": msg }));
}

/// Готовый результат рендера.
#[allow(dead_code)]
pub struct RenderResult {
    pub output: PathBuf,
}

/// Отрендерить Project -> output.mp4. `regen_dub` — ре-синтез dirty-сегментов дубляжа (иначе кэш).
pub fn run(
    proj: &Project,
    paths: &RenderPaths,
    regen_dub: bool,
    progress: &Progress,
) -> Result<RenderResult, String> {
    dub_captions::set_fonts_dir(&paths.fonts_dir);
    let wd = &paths.work_dir;
    std::fs::create_dir_all(wd).map_err(|e| e.to_string())?;

    // probe: длительность/размеры.
    let meta = media::probe(&paths.input)?;
    let total = if proj.meta.duration > 0.0 {
        proj.meta.duration
    } else {
        meta.duration
    };
    let (vw, vh) = (
        if proj.meta.width > 0 { proj.meta.width } else { meta.width },
        if proj.meta.height > 0 { proj.meta.height } else { meta.height },
    );
    let src_codec = if !proj.meta.src_codec.is_empty() {
        proj.meta.src_codec.clone()
    } else {
        meta.src_codec.clone()
    };
    emit(progress, "probe", &format!("вход {}x{} dur={:.1}s", vw, vh, total));

    // voiceover (закадровый) = как dub, но оригинал слышно приглушённым ПОД переведённым голосом.
    let is_voiceover = proj.mode == "voiceover";
    let is_dub = proj.mode == "dub" || is_voiceover;
    let keep_music = proj.audio.keep_music;

    // ── АУДИО ──────────────────────────────────────────────────────────────────
    // Готовим финальную аудио-дорожку new_audio: dub (клон) поверх инструментала, либо оригинал.
    let new_audio: PathBuf = if is_dub {
        build_dub(proj, paths, total, keep_music, is_voiceover, regen_dub, progress)?
    } else {
        // nodub/transcribe: оставляем оригинальную дорожку — mux возьмёт её из исходного видео.
        emit(progress, "mix", "nodub: оригинальная аудиодорожка");
        paths.input.clone()
    };

    // ── КАПШЕНЫ + BURN (только если subs.burn) ─────────────────────────────────
    // subs.burn=false -> НИКАКИХ наложений (ни субтитров, ни титров/блюра): чистое видео + новая
    // дорожка. Композируемость: дубляж/закадр без субтитров на картинке.
    // Есть ли ВООБЩЕ что накладывать? subs=none + нет титров + нет блюр-боксов (band уже исключён при
    // subs=none) -> накладывать нечего, полный ffmpeg-транскод бессмыслен (экономия времени, баг-репорт).
    let has_overlay = proj.subs.mode != "none"
        || !proj.captions.titles.is_empty()
        || !collect_blur_boxes(proj).is_empty();
    let captioned = if proj.subs.burn && has_overlay {
        emit(progress, "build", "сборка ASS (титры + дублированные субтитры)");
        let ass_path = wd.join("caps.ass");
        let sub_covers = build_ass(proj, &ass_path, vw, vh, total)?;
        emit(progress, "burn", "вжигание субтитров + блюр (ffmpeg + libass, NVENC)");
        let mut blur_boxes = collect_blur_boxes(proj);
        blur_boxes.extend(sub_covers.iter().map(cover_to_blur)); // блюр-подложка ПОД нашим текстом
        let captioned = wd.join("captioned.mp4");
        dub_captions::burn(
            &paths.input,
            &ass_path,
            &captioned,
            &blur_boxes,
            Some((vw, vh)),
            proj.render.blur,
            true, // gpu_encode (NVENC)
            true, // gpu_decode
            proj.render.burn_cq,
            Some(&src_codec),
            proj.render.blur_sigma,
        )?;
        captioned
    } else {
        emit(progress, "burn", "субтитры/титры отключены (subs.burn=off)");
        paths.input.clone()
    };

    // ── MUX ────────────────────────────────────────────────────────────────────
    emit(progress, "mux", "муксирование видео + аудио");
    if is_dub || new_audio != paths.input {
        media::mux(&captioned, &new_audio, &paths.output)?;
    } else {
        // nodub: тянем аудио из исходного видео (captioned без звука) -> mux исходной дорожки.
        media::mux(&captioned, &paths.input, &paths.output)?;
    }

    emit(progress, "done", &format!("готово -> {}", paths.output.display()));
    Ok(RenderResult { output: paths.output.clone() })
}

/// Только ДУБ-АУДИО (без бёрна субтитров и mux видео): TTS+fit+timeline+mix -> work_dir/dub_audio.m4a.
/// Нужно, чтобы озвучку можно было СЛУШАТЬ в редакторе сразу после анализа, НЕ собирая финальное видео
/// (само видео на превью не нужно — кадры показывает per-frame preview). Порт _build_dub-ветки без бёрна.
pub fn dub_audio(
    proj: &Project,
    paths: &RenderPaths,
    regen_dub: bool,
    progress: &Progress,
) -> Result<PathBuf, String> {
    let wd = &paths.work_dir;
    std::fs::create_dir_all(wd).map_err(|e| e.to_string())?;
    let meta = media::probe(&paths.input)?;
    let total = if proj.meta.duration > 0.0 { proj.meta.duration } else { meta.duration };
    let src: PathBuf = if proj.mode == "dub" || proj.mode == "voiceover" {
        build_dub(proj, paths, total, proj.audio.keep_music, proj.mode == "voiceover", regen_dub, progress)?
    } else {
        paths.input.clone() // nodub/transcribe -> оригинальная дорожка
    };
    let out = wd.join("dub_audio.m4a");
    // привести к browser-playable aac/m4a (build_dub уже даёт m4a; nodub -> извлечь звук из оригинала).
    media::extract_audio(&src, &out, 44_100, 2)?;
    emit(progress, "done", "дуб-аудио готово");
    Ok(out)
}

/// Нижний предел приглушения оригинала в режиме voiceover (закадровый): -40 dB ≈ почти тихо.
/// Само значение регулирует пользователь (proj.audio.voiceover_gain_db, дефолт -6 dB).
const VOICEOVER_DUCK_MIN_DB: f64 = -40.0;

/// Полный аудио-конвейер дубляжа -> путь к new_audio. Порт _build_dub/_regen_dub (TTS+fit+timeline+mix).
fn build_dub(
    proj: &Project,
    paths: &RenderPaths,
    total: f64,
    keep_music: bool,
    voiceover: bool,
    regen_dub: bool,
    progress: &Progress,
) -> Result<PathBuf, String> {
    let wd = &paths.work_dir;
    // Сегменты с непустым tgt (как в питоне: только строки с текстом синтезируются). Несём индекс в
    // ПОЛНОМ списке proj.segments — слот next.start считается по индексу i+1 полного списка (порт
    // pipeline.py:207: nxt = segs[i+1].start, где segs — весь транскрипт, пустые пропускаются continue,
    // но индекс i+1 идёт по полному списку).
    // Порт project.write_artifacts: HIDDEN строки исключаются целиком (нет ни дубляжа, ни субтитра);
    // keep_original — остаются (сплайсим оригинал, без TTS), даже если tgt непустой.
    let seg_hidden = |s: &dub_core::Segment| s.extra.get("hidden").and_then(|v| v.as_bool()).unwrap_or(false);
    let seg_keep = |s: &dub_core::Segment| s.extra.get("keep_original").and_then(|v| v.as_bool()).unwrap_or(false);
    let segs: Vec<(usize, &dub_core::Segment)> = proj
        .segments
        .iter()
        .enumerate()
        .filter(|(_, s)| !seg_hidden(s) && (seg_keep(s) || !s.tgt_text.trim().is_empty()))
        .collect();
    if segs.is_empty() {
        emit(progress, "tts", "нет строк с переводом -> тишина, оригинальная дорожка");
        return Ok(paths.input.clone());
    }

    // 1) extract 44.1k stereo.
    emit(progress, "extract_audio", "извлечение аудио (ffmpeg 44.1k stereo)");
    let audio_hq = wd.join("audio_hq.wav");
    media::extract_audio(&paths.input, &audio_hq, 44100, 2)?;

    // 2) сепарация (vocals/instrumental) через dub-sep, если keep_music.
    //    voiceover не сепарирует: нужен ВЕСЬ оригинал (голос+музыка) приглушённым под переводом.
    let (vocals, instrumental): (PathBuf, Option<PathBuf>) = if keep_music && !voiceover {
        emit(progress, "separate", "сепарация (Mel-Band Roformer voc_fv6-Q8_0)");
        if paths.bsroformer_cli.is_file() && paths.bsroformer_model.is_file() {
            let sep = dub_sep::separate(
                &audio_hq,
                &wd.join("stems"),
                &paths.bsroformer_cli,
                &paths.bsroformer_model,
            )
            .map_err(|e| format!("сепарация: {e}"))?;
            (sep.vocals, Some(sep.instrumental))
        } else {
            emit(progress, "separate", "движок сепарации не найден -> без фона (keep_music off)");
            (audio_hq.clone(), None)
        }
    } else {
        (audio_hq.clone(), None)
    };

    // vocals16 -> mono 16k (референсы клона).
    let vocals16 = wd.join("vocals16.wav");
    media::to_16k_mono(&vocals, &vocals16)?;

    // 3) клон-референс. voice.mode="voice" -> реф из пака/записи (voices/<name>.wav|mp3) НА КАЖДОГО спикера.
    //    Имя — CSV; позиция = отсортированный спикер (как во фронте: speaker ?? "0", лексикографически),
    //    пустой слот берёт первое непустое имя. Иначе (clone) — длиннейшая реплика спикера из вокала.
    let pack_refs: std::collections::BTreeMap<String, PathBuf> = if proj.audio.voice.mode == "voice" {
        let names: Vec<&str> = proj.audio.voice.name.as_deref().unwrap_or("").split(',').map(|s| s.trim()).collect();
        let first_named = names.iter().copied().find(|n| !n.is_empty());
        let mut map = std::collections::BTreeMap::new();
        if first_named.is_some() {
            let sorted: Vec<String> = proj
                .segments
                .iter()
                .map(|s| s.speaker.clone().unwrap_or_else(|| "0".to_string()))
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            for (i, spk) in sorted.iter().enumerate() {
                let nm = names.get(i).copied().filter(|s| !s.is_empty()).or(first_named).unwrap_or("");
                let src = ["wav", "mp3"].iter().map(|e| paths.voices_dir.join(format!("{nm}.{e}"))).find(|p| p.is_file());
                if let Some(src) = src {
                    let out = wd.join(format!("ref_pack_{i}.wav"));
                    // реф КАПИТСЯ до paths.ref_secs (дефолт 12с; на слабой RAM юзер уменьшает в настройках —
                    // длинный реф раздувает prefill-граф Higgs -> OOM на 32ГБ).
                    if media::trim(&src, &out, 0.0, paths.ref_secs, 16_000).is_ok() {
                        map.insert(spk.clone(), out);
                    }
                }
            }
        }
        map
    } else {
        std::collections::BTreeMap::new()
    };
    let use_pack = !pack_refs.is_empty();
    // ref_texts: расшифровка реф-клипа НА СПИКЕРА (Higgs клонирует качественнее с ref_text). Клон-режим —
    // src_text выбранного сегмента; пак-режим — АВТОТРАНСКРИПЦИЯ 12с-клипа (как Higgs build_speaker_reference;
    // пак-.txt = полный 3-мин транскрипт, к 12с не подходит). ASR best-effort: сбой -> None (не хуже прежнего).
    let (spk_refs, mut ref_texts) = if use_pack {
        (std::collections::BTreeMap::new(), std::collections::BTreeMap::new())
    } else {
        build_speaker_refs(&segs, &vocals16, wd, paths.ref_secs)?
    };
    if use_pack {
        // Реф-транскрипция выбранным движком (Parakeet/Whisper), а НЕ захардкоженным Parakeet — иначе у
        // Whisper-only юзера (без Parakeet-модели) ref_text молча не считался бы. build_engine сам решает.
        let mut asr = crate::models::build_engine(&paths.asr);
        for (spk, refp) in &pack_refs {
            if let Ok(rsegs) = asr.transcribe(refp, "auto") {
                let txt = rsegs
                    .iter()
                    .map(|s| s.text.trim())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
                if !txt.trim().is_empty() {
                    ref_texts.insert(spk.clone(), txt);
                }
            }
        }
    }
    // КЛОН: реф-клипы, обрезанные из-за уменьшенного ref_secs (в build_speaker_refs текст НЕ выставлен),
    // ПЕРЕтранскрибируем — чтобы ref_text совпал с укороченным реф-аудио (иначе клон рассинхронится).
    // Реф не обрезан (текст уже есть) -> не трогаем, поведение по умолчанию (12с) неизменно.
    if !use_pack {
        let mut asr = crate::models::build_engine(&paths.asr);
        for (spk, refp) in &spk_refs {
            if ref_texts.contains_key(spk) {
                continue;
            }
            if let Ok(rsegs) = asr.transcribe(refp, "auto") {
                let txt = rsegs
                    .iter()
                    .map(|s| s.text.trim())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
                if !txt.trim().is_empty() {
                    ref_texts.insert(spk.clone(), txt);
                }
            }
        }
    }
    let first_ref = pack_refs
        .values()
        .next()
        .cloned()
        .or_else(|| spk_refs.values().next().cloned())
        .unwrap_or_else(|| wd.join("ref.wav"));
    let ref_of = |s: &dub_core::Segment| -> PathBuf {
        let key = s.speaker.as_deref().unwrap_or("0");
        if use_pack {
            return pack_refs.get(key).cloned().unwrap_or_else(|| first_ref.clone());
        }
        s.speaker
            .as_deref()
            .and_then(|k| spk_refs.get(k))
            .cloned()
            .unwrap_or_else(|| first_ref.clone())
    };
    let reftext_of = |s: &dub_core::Segment| -> Option<String> {
        let key = s.speaker.as_deref().unwrap_or("0");
        if let Some(t) = ref_texts.get(key) {
            return Some(t.clone());
        }
        // у спикера есть СВОЁ реф-аудио, но текста нет -> None (чужой текст к чужому аудио хуже, чем без
        // текста). Спикер без своего аудио (fit -> first_ref) берёт текст первого спикера (совпадает с ним).
        let has_own = if use_pack { pack_refs.contains_key(key) } else { spk_refs.contains_key(key) };
        if has_own {
            None
        } else {
            ref_texts.values().next().cloned()
        }
    };

    // 4) TTS каждый сегмент через Higgs (audiocpp). Кэш: seg_XXX.wav; не-dirty переиспользуются.
    emit(progress, "tts", &format!("синтез {} сегментов (Higgs clone)", segs.len()));
    let engine = AudiocppEngine::load(&paths.higgs_dll)
        .map_err(|e| format!("загрузка Higgs DLL: {e}"))?;
    engine
        .load_model(
            &paths.higgs_model_root,
            &paths.higgs_backend,
            paths.higgs_device,
            paths.higgs_threads,
            Some(paths.higgs_quant.as_str()),
        )
        .map_err(|e| format!("Higgs load_model: {e}"))?;

    // placed = [(at, wav_path)]. cursor-aware fit (как в питоне). Имя seg-файла и слот next.start —
    // по индексу fi в ПОЛНОМ списке proj.segments (питон: seg_{i:03d}.wav, nxt = segs[i+1].start).
    let mut placed: Vec<(f64, PathBuf)> = Vec::with_capacity(segs.len());
    let mut cursor = 0.0f64;
    let n_all = proj.segments.len();
    for &(fi, s) in segs.iter() {
        // Кэш-файл сегмента — ПО ЕГО ID, не по индексу fi. Кэш переиспользуется между рендерами (не-dirty
        // сегменты не ре-синтезируются). При индекс-имени удаление/перестановка сегмента сдвигает индексы —
        // и чистый сегмент подхватил бы seg_{fi}.wav ПРЕДЫДУЩЕГО жильца индекса => чужая речь/длительность =
        // ДРИФТ дубляжа (регресс кэша порта; питон синтезил заново каждый рендер). ID стабилен -> кэш привязан
        // к контенту. Слот next.start (nxt) остаётся по индексу — это про таймлайн-позицию, не про кэш.
        let sid: String = s.id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        let sid = if sid.is_empty() { format!("i{fi}") } else { sid };
        let raw = wd.join(format!("seg_{sid}.wav"));
        // 'оставить оригинал': вырезаем ИСХОДНУЮ речь сюда, без TTS и без atempo-подгонки (порт _build_dub keep-ветки).
        // Режем СРАЗУ в 24к моно (питон media.trim(..., sr=24000)) — timeline кладёт по sr ПЕРВОГО файла (TTS=24к),
        // без ресемпла; 44.1к-вырез играл бы не на той скорости. Без промежуточного 16к (не терять ВЧ).
        if seg_keep(s) {
            media::trim(&vocals, &raw, s.start, s.end, 24_000)?;
            let at = s.start.max(cursor);
            cursor = at + media::duration(&raw)?;
            placed.push((at, raw));
            continue;
        }
        let tgt = s.tgt_text.trim();
        let ref_wav = ref_of(s);
        // Синтез ТОЛЬКО если сегмент dirty (правился текст/спикер/голос) ИЛИ нет кэша. Реф-клипы
        // пересобираются каждый рендер, поэтому mtime-сравнение с рефом («stale_ref») ошибочно
        // помечало ВЕСЬ кэш устаревшим на каждом рендере → экспорт ре-роллил уже одобренную озвучку
        // («скидывалось»). Смена голоса и так метит все сегменты dirty (op_recast/op_segment), так что
        // dirty-флага достаточно: не-dirty сегменты переиспользуют свой seg_XXX.wav между рендерами.
        let need_synth = (regen_dub && s.dirty) || !raw.is_file();
        if need_synth {
            let ref_text = reftext_of(s); // авто-расшифровка рефа -> качество клона (как Higgs)
            let (samples, sr) = engine
                .voice_clone(tgt, &ref_wav.to_string_lossy(), ref_text.as_deref(), "")
                .map_err(|e| format!("Higgs clone seg{fi}: {e}"))?;
            let wav = AudiocppEngine::encode_wav(&samples, sr, 1);
            std::fs::write(&raw, &wav).map_err(|e| format!("запись seg{fi}: {e}"))?;
        }
        // слот: от текущего onset до старта СЛЕДУЮЩЕГО сегмента ПО ИНДЕКСУ (fi+1) полного списка /
        // конца видео (питон nxt = segs[i+1].start if i+1<len else total).
        let at = s.start.max(cursor);
        let nxt = if fi + 1 < n_all { proj.segments[fi + 1].start } else { total };
        let room = (nxt - at).max(0.3);
        let fit = fit_to_slot(&raw, room, &wd.join(format!("seg_{:03}_fit.wav", fi)), paths.max_stretch)?;
        cursor = at + media::duration(&fit)?;
        placed.push((at, fit));
    }

    // 5) timeline -> dub_vocals.wav.
    emit(progress, "mix", "укладка дубляжа на таймлайн");
    let dub = wd.join("dub_vocals.wav");
    timeline(&placed, total, &dub)?;
    // HARD-гарантия: дубляж не длиннее видео (tempo-fit всей дорожки, если переполз).
    let mut dub = dub;
    let dub_dur = media::duration(&dub)?;
    if dub_dur > total + 0.15 {
        let fit = wd.join("dub_fit.wav");
        media::time_stretch(&dub, &fit, dub_dur / total)?;
        emit(progress, "mix", &format!("tempo-fit всей дорожки x{:.2}", dub_dur / total));
        dub = fit;
    }

    // 6) свести дорожку.
    let mixed = if voiceover {
        // Закадровый: весь оригинал приглушаем на voiceover_gain_db (регулируется в редакторе) и кладём
        // ПОД переведённый голос. Слышно исходного спикера под дублем (эффект «voice-over»). loudnorm
        // ниже приведёт программу к -14 LUFS — соотношение дубль/оригинал сохранится.
        let duck_db = proj.audio.voiceover_gain_db.clamp(VOICEOVER_DUCK_MIN_DB, 0.0);
        emit(progress, "mix", &format!("voiceover: оригинал {duck_db:+.1} dB под переводом"));
        // .m4a: media::gain кодирует в AAC — расширение должно совпадать (AAC в .wav-контейнере
        // читается как тишина). amix(normalize=0) суммирует дубль (полный) + оригинал (приглушённый).
        // duck_db==0 -> gain не нужен, кладём оригинал как есть.
        let bed = if duck_db.abs() < 0.05 {
            audio_hq.clone()
        } else {
            let ducked = wd.join("orig_ducked.m4a");
            match media::gain(&audio_hq, &ducked, duck_db) {
                Ok(()) => ducked,
                Err(_) => audio_hq.clone(),
            }
        };
        let new_audio = wd.join("new_audio.m4a");
        media::mix(&dub, &bed, &new_audio)?;
        new_audio
    } else if let Some(inst) = instrumental {
        emit(progress, "mix", "сведение: инструментал + дубль-вокал");
        let new_audio = wd.join("new_audio.m4a");
        media::mix(&dub, &inst, &new_audio)?;
        new_audio
    } else {
        dub
    };
    // 7) финальная нормализация программы EBU R128 + true-peak лимитер (-1 dBTP). РЕШЕНИЕ ЮЗЕРА
    // (best-practice, НЕ питон — приказ 2026-07-12): пофразный normalize_voice выровнял спикеров, здесь
    // программа приводится к целевой громкости соцсетей (-14 LUFS) БЕЗ клиппинга. Пики держит именно этот
    // лимитер (поэтому normalize_voice без пофразного peak-клэмпа — он мешал дожать тихие фразы).
    emit(progress, "mix", "нормализация громкости (EBU R128, true-peak)");
    let final_audio = wd.join("final_audio.m4a");
    let normalized = match media::loudnorm(&mixed, &final_audio, -14.0, -1.0, 11.0) {
        Ok(()) => final_audio,
        Err(e) => {
            emit(progress, "mix", &format!("loudnorm пропущен ({e})"));
            mixed
        }
    };
    // 8) монтажный гейн всей дорожки (если задан) — наша opt-in фича «усилить всё» поверх нормализации.
    let gain_db = proj.audio.gain_db;
    if gain_db.abs() > 0.05 {
        emit(progress, "mix", &format!("гейн дорожки {gain_db:+.1} dB"));
        let gained = wd.join("gained_audio.m4a");
        match media::gain(&normalized, &gained, gain_db) {
            Ok(()) => Ok(gained),
            Err(_) => Ok(normalized),
        }
    } else {
        Ok(normalized)
    }
}

/// Референс клона на КАЖДОГО спикера: {speaker -> ref_spk{N}.wav} из его длиннейшей реплики (<=12с).
/// Порт voices.resolve clone-ветки. Спикер None -> ключ "0" (моно-ролик = один реф).
type SpkRefs = (
    std::collections::BTreeMap<String, PathBuf>,
    std::collections::BTreeMap<String, String>,
);

fn build_speaker_refs(
    segs: &[(usize, &dub_core::Segment)],
    vocals16: &Path,
    wd: &Path,
    ref_secs: f64,
) -> Result<SpkRefs, String> {
    let mut refs: std::collections::BTreeMap<String, PathBuf> = std::collections::BTreeMap::new();
    // ref_text клона = src_text выбранного сегмента (сегменты ≤8с -> совпадает с ≤12с реф-клипом).
    let mut texts: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    let mut speakers: Vec<String> =
        segs.iter().map(|(_, s)| s.speaker.clone().unwrap_or_else(|| "0".into())).collect();
    speakers.sort();
    speakers.dedup();
    for spk in speakers {
        let cand = segs
            .iter()
            .filter(|(_, s)| s.speaker.clone().unwrap_or_else(|| "0".into()) == spk)
            .max_by(|(_, a), (_, b)| (a.end - a.start).partial_cmp(&(b.end - b.start)).unwrap())
            .map(|(_, s)| *s);
        let Some(cand) = cand else { continue };
        let ref_wav = wd.join(format!("ref_spk{spk}.wav"));
        media::trim(vocals16, &ref_wav, cand.start, cand.end.min(cand.start + ref_secs), 16_000)?;
        refs.insert(spk.clone(), ref_wav);
        // ref_text = src_text сегмента, НО только если реф НЕ обрезан (сегмент ≤ ref_secs). Если юзер
        // уменьшил ref_secs и реф стал короче сегмента, полный src_text описывает БОЛЬШЕ, чем в обрезанном
        // аудио -> рассинхрон клона. Такие оставляем БЕЗ текста -> ПЕРЕтранскрибируем обрезанный клип ниже.
        let t = cand.src_text.trim();
        if !t.is_empty() && (cand.end - cand.start) <= ref_secs + 0.05 {
            texts.insert(spk, t.to_string());
        }
    }
    Ok((refs, texts))
}

/// Ускорить дубль под target_dur, если он длиннее (никогда не замедлять). Порт assemble.fit_to_slot.
fn fit_to_slot(seg_wav: &Path, target_dur: f64, work_path: &Path, max_stretch: f64) -> Result<PathBuf, String> {
    let actual = media::duration(seg_wav)?;
    if target_dur <= 0.05 || actual <= 0.05 {
        return Ok(seg_wav.to_path_buf());
    }
    let mut factor = actual / target_dur;
    factor = factor.min(max_stretch).max(1.0);
    if factor <= 1.02 {
        return Ok(seg_wav.to_path_buf());
    }
    media::time_stretch(seg_wav, work_path, factor)?;
    Ok(work_path.to_path_buf())
}

/// Уложить сегменты на полную дорожку по таймкодам, без перекрытия/обрезки. Порт assemble.timeline.
fn timeline(placed: &[(f64, PathBuf)], total_dur: f64, out_wav: &Path) -> Result<(), String> {
    if placed.is_empty() {
        // тишина total_dur @ 24000.
        let n = (total_dur * 24000.0) as usize;
        wavio::write_mono_f32(out_wav, &vec![0.0f32; n], 24000)?;
        return Ok(());
    }
    let mut placed: Vec<(f64, PathBuf)> = placed.to_vec();
    placed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    // sr берём из первого файла.
    let first = wavio::read_mono_f32(&placed[0].1)?;
    let sr = first.1;
    let mut laid: Vec<(f64, Vec<f32>)> = Vec::with_capacity(placed.len());
    let mut cursor = 0.0f64;
    for (start, wav) in &placed {
        let (mut s, ssr) = if *wav == placed[0].1 {
            (first.0.clone(), first.1)
        } else {
            wavio::read_mono_f32(wav)?
        };
        normalize_voice(&mut s, ssr); // все фразы/спикеры к одной громкости
        let at = start.max(cursor);
        cursor = at + s.len() as f64 / sr as f64;
        laid.push((at, s));
    }
    let len = ((total_dur.max(cursor) + 0.5) * sr as f64) as usize;
    let mut track = vec![0.0f32; len];
    for (at, s) in &laid {
        let i = (at * sr as f64) as usize;
        let end = (i + s.len()).min(track.len());
        for (k, v) in s.iter().take(end - i).enumerate() {
            track[i + k] += *v;
        }
    }
    let peak = track.iter().fold(0.0f32, |m, &x| m.max(x.abs())).max(1.0);
    if peak > 1.0 {
        for x in &mut track {
            *x /= peak;
        }
    }
    wavio::write_mono_f32(out_wav, &track, sr)?;
    Ok(())
}

/// Выровнять ОДНУ фразу к общей громкости, чтобы все спикеры звучали одинаково громко (dialog-gated
/// нормализация): интегральная громкость BS.1770 к -16 LUFS; короткие/тихие фразы — RMS к -18 dBFS.
/// РЕШЕНИЕ ЮЗЕРА (EBU R128 best-practice, НЕ копия питона — «гугли best practices, не повторяй за мной»,
/// приказ 2026-07-12): БЕЗ пофразного peak-клэмпа (он мешал дожать тихую фразу) — пики держит финальный
/// true-peak лимитер media::loudnorm на смиксованной дорожке. Сани-кап +40 dB (не раздувать почти-тишину).
fn normalize_voice(x: &mut [f32], sr: u32) {
    if x.is_empty() {
        return;
    }
    let peak = x.iter().fold(0.0f32, |m, &v| m.max(v.abs()));
    if peak < 1e-4 {
        return; // почти тишина -> не трогаем
    }
    let mut gain: Option<f64> = None;
    if x.len() >= (0.4 * sr as f64) as usize {
        if let Some(li) = integrated_lufs(x, sr) {
            if li.is_finite() && li > -60.0 {
                gain = Some(10f64.powf((-16.0 - li) / 20.0));
            }
        }
    }
    let gain = gain.unwrap_or_else(|| {
        let thr = peak * 0.05;
        let (mut sum, mut cnt) = (0.0f64, 0usize);
        for &v in x.iter() {
            if v.abs() > thr {
                sum += (v as f64) * (v as f64);
                cnt += 1;
            }
        }
        let rms = if cnt > 0 {
            (sum / cnt as f64).sqrt()
        } else {
            (x.iter().map(|&v| (v as f64) * (v as f64)).sum::<f64>() / x.len() as f64).sqrt()
        }
        .max(1e-9);
        10f64.powf(-18.0 / 20.0) / rms
    });
    let gain = gain.min(10f64.powf(40.0 / 20.0)); // сани-кап +40 dB; пики ловит финальный loudnorm (НЕ пофразно)
    for v in x.iter_mut() {
        *v = (*v as f64 * gain) as f32;
    }
}

/// Интегральная громкость ITU-R BS.1770 (LUFS) моно-сигнала: K-weighting (high-shelf + high-pass
/// биквады как в pyloudnorm) -> блоки 400мс с overlap 75% -> абсолютный гейт -70 + относительный -10.
/// None если блоков не осталось (слишком коротко/тихо).
fn integrated_lufs(x: &[f32], sr: u32) -> Option<f64> {
    let hs = biquad_high_shelf(1681.9744509555319, 0.7071752369554196, 4.0, sr as f64);
    let hp = biquad_high_pass(38.13547087613982, 0.5003270373253953, sr as f64);
    let y = apply_biquad(&apply_biquad(x, &hs), &hp);
    let block = (0.4 * sr as f64) as usize;
    let step = (0.1 * sr as f64) as usize;
    if block == 0 || step == 0 || y.len() < block {
        return None;
    }
    let mut zs: Vec<f64> = Vec::new();
    let mut i = 0;
    while i + block <= y.len() {
        let ms: f64 = y[i..i + block].iter().map(|&v| (v as f64) * (v as f64)).sum::<f64>()
            / block as f64;
        zs.push(ms);
        i += step;
    }
    if zs.is_empty() {
        return None;
    }
    let loud = |z: f64| -0.691 + 10.0 * (z.max(1e-12)).log10();
    // абсолютный гейт -70 LUFS
    let abs_gated: Vec<f64> = zs.iter().copied().filter(|&z| loud(z) >= -70.0).collect();
    if abs_gated.is_empty() {
        return None;
    }
    let mean_abs = abs_gated.iter().sum::<f64>() / abs_gated.len() as f64;
    let rel_thr = loud(mean_abs) - 10.0;
    let rel_gated: Vec<f64> = abs_gated.into_iter().filter(|&z| loud(z) >= rel_thr).collect();
    if rel_gated.is_empty() {
        return None;
    }
    let mean_rel = rel_gated.iter().sum::<f64>() / rel_gated.len() as f64;
    Some(loud(mean_rel))
}

/// Биквад-фильтр прямой формы I (b/a нормированы на a0). Возвращает отфильтрованный сигнал.
fn apply_biquad(x: &[f32], c: &[f64; 5]) -> Vec<f32> {
    let [b0, b1, b2, a1, a2] = *c;
    let (mut x1, mut x2, mut y1, mut y2) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    let mut out = Vec::with_capacity(x.len());
    for &xn in x {
        let xn = xn as f64;
        let yn = b0 * xn + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
        x2 = x1;
        x1 = xn;
        y2 = y1;
        y1 = yn;
        out.push(yn as f32);
    }
    out
}

/// High-shelf биквад (pyloudnorm K-weighting stage 1). Коэффы [b0,b1,b2,a1,a2] нормированы на a0.
fn biquad_high_shelf(fc: f64, q: f64, gain_db: f64, sr: f64) -> [f64; 5] {
    let a = 10f64.powf(gain_db / 40.0);
    let w0 = 2.0 * std::f64::consts::PI * fc / sr;
    let (cw, sw) = (w0.cos(), w0.sin());
    let alpha = sw / (2.0 * q);
    let am = a - 1.0;
    let ap = a + 1.0;
    let sa = 2.0 * a.sqrt() * alpha;
    let b0 = a * (ap + am * cw + sa);
    let b1 = -2.0 * a * (am + ap * cw);
    let b2 = a * (ap + am * cw - sa);
    let a0 = ap - am * cw + sa;
    let a1 = 2.0 * (am - ap * cw);
    let a2 = ap - am * cw - sa;
    [b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0]
}

/// High-pass биквад (pyloudnorm K-weighting stage 2, RLB). Коэффы нормированы на a0.
fn biquad_high_pass(fc: f64, q: f64, sr: f64) -> [f64; 5] {
    let w0 = 2.0 * std::f64::consts::PI * fc / sr;
    let (cw, sw) = (w0.cos(), w0.sin());
    let alpha = sw / (2.0 * q);
    let b0 = (1.0 + cw) / 2.0;
    let b1 = -(1.0 + cw);
    let b2 = (1.0 + cw) / 2.0;
    let a0 = 1.0 + alpha;
    let a1 = -2.0 * cw;
    let a2 = 1.0 - alpha;
    [b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0]
}

/// E2E-верификация капшенов без TTS/аудио: собрать ASS из Project и вжечь блюр+сабы в captioned.mp4.
/// Используется примером verify_captions (реальные кадры порт-vs-эталон). Порт captions.build+burn
/// call-site pipeline.run, но БЕЗ аудио-ветки.
pub(crate) fn build_and_burn_captions(
    proj: &Project,
    input: &Path,
    out_ass: &Path,
    captioned: &Path,
    fonts_dir: &Path,
    vw: i64,
    vh: i64,
    total: f64,
    src_codec: &str,
) -> Result<(), String> {
    dub_captions::set_fonts_dir(fonts_dir);
    let sub_covers = build_ass(proj, out_ass, vw, vh, total)?;
    let mut blur_boxes = collect_blur_boxes(proj);
    blur_boxes.extend(sub_covers.iter().map(cover_to_blur));
    dub_captions::burn(
        input,
        out_ass,
        captioned,
        &blur_boxes,
        Some((vw, vh)),
        proj.render.blur,
        true,
        true,
        proj.render.burn_cq,
        Some(src_codec),
        proj.render.blur_sigma,
    )
}

/// Блюр-подложка под нашим субтитром -> BlurBox (fill=None -> gblur). Старые band-боксы не трогаем.
pub(crate) fn cover_to_blur(c: &dub_captions::SubCover) -> BlurBox {
    BlurBox { x: c.x, y: c.y, w: c.w, h: c.h, t0: c.t0, t1: c.t1, fill: None }
}

/// Собрать ASS через dub-captions из Project. Порт captions.build call-site pipeline.run. Возвращает
/// габариты подложек под дублированными субтитрами (для блюр-подложки; см. SubCover).
pub(crate) fn build_ass(proj: &Project, out_ass: &Path, vw: i64, vh: i64, total: f64) -> Result<Vec<dub_captions::SubCover>, String> {
    let titles: Vec<CapTitle> = proj.captions.titles.iter().map(map_title).collect();
    let sub_style = proj.captions.sub_style.as_ref().map(map_sub_style);
    // sub_y дефолт vh*0.82 если не задан (как pipeline.py: не затирать edited/pinned sub_y).
    let sub_y = proj.captions.sub_y.unwrap_or((vh as f64 * 0.82) as i64);
    // PER-SEGMENT Y-RIDE (порт pipeline.py 616-631). Каждую дублированную строку кладём на y, где в этот
    // момент была ОРИГИНАЛЬНАЯ полоса сабов, чтобы наш текст/плашка НАКРЫЛИ заблюренный оригинал (а не
    // висели на одной фикс-линии, пока блюр другой строки просвечивает). Источник полосы —
    // persisted blur_boxes. Питон медианит per-segment seg_y (pipeline.py 757-765) ТОЛЬКО по caption_boxes
    // (субтитр-полоса из analyze_layout), а НЕ по всему blur-набору — титры/таглайны/group туда не входят.
    // Порт складывает всё в blur_boxes, поэтому band-подмножество помечено compose.rs маркером extra["band"]
    // (= питоновский caption_boxes-производный band_blur). seg_y едет ТОЛЬКО по нему. Без такого — верхний
    // title/tagline-блюр в нижней половине кадра затягивал медиану вверх и строка садилась мимо полосы
    // Fallback (старые проекты без
    // маркера / ручной blur из редактора): весь blur-набор, как было — иначе потеряли бы band целиком.
    let all_band: Vec<&dub_core::BlurBox> =
        proj.captions.blur_boxes.iter().filter(|b| !b.hidden).collect();
    let tagged: Vec<&dub_core::BlurBox> = all_band
        .iter()
        .copied()
        .filter(|b| b.extra.get("band").and_then(|v| v.as_bool()).unwrap_or(false))
        .collect();
    let band: Vec<&dub_core::BlurBox> = if tagged.is_empty() { all_band } else { tagged };
    let no_band = band.len() < 3; // нет повторяющейся ОРИГИНАЛЬНОЙ полосы -> не на что ехать
    let cap_lo = 0.40 * vw as f64;
    let cap_hi = 0.60 * vw as f64;
    let seg_y = |st: f64, en: f64| -> i64 {
        if proj.captions.sub_y_locked || no_band {
            return sub_y; // editor-pinned или полосы нет -> выбранная band
        }
        // медиана y-центров band-боксов, перекрывающих сегмент по времени, центрированных по X,
        // в нижней половине кадра (ехать на нижнюю оригинальную полосу, не на верхний оверлей).
        let mut ys: Vec<f64> = band
            .iter()
            .filter(|b| {
                let cyb = b.y as f64 + b.h as f64 / 2.0;
                (b.t0 as f64) < en + 0.3
                    && (b.t1 as f64) > st - 0.3
                    && (b.x as f64) < cap_hi
                    && (b.x as f64 + b.w as f64) > cap_lo
                    && cyb >= 0.45 * vh as f64
            })
            .map(|b| b.y as f64 + b.h as f64 / 2.0)
            .collect();
        if ys.is_empty() {
            return (vh as f64 * 0.82) as i64;
        }
        ys.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        ys[ys.len() / 2] as i64
    };
    // Per-segment CaptionOverride: карта seg_id -> текст-оверрайд (редактор дал свой текст строки).
    // Порт продуктового требования «научить build_ass читать overrides»: питоновский write_artifacts
    // overrides в план НЕ прокидывает (хранит round-trip), поэтому текст-оверрайд — Rust-улучшение поверх
    // источника истины: если для сегмента задан override.text, рисуем ЕГО вместо tgt_text. Стилевые
    // per-seg поля (override.style/x/y/w/fs) сохраняются в Project, но в ASS-строку пока не вплетаются —
    // dub-captions строит субтитр из общего sub_style; это совпадает с питоном (тоже не рендерит их).
    let overrides: std::collections::HashMap<&str, &str> = proj
        .captions
        .overrides
        .iter()
        .filter_map(|o| o.text.as_deref().map(|t| (o.seg_id.as_str(), t)))
        .collect();
    // Режим «без субтитров» (subs.mode=none) -> НЕ рисуем строки субтитров вообще. Титры/локализация
    // экранного текста живут отдельно (proj.captions.titles) и не затрагиваются. Раньше build_ass
    // рендерил сегменты безусловно -> в режиме «без субтитров» они всё равно прожигались (баг-репорт).
    let subs: Vec<Sub> = if proj.subs.mode == "none" {
        Vec::new()
    } else { proj
        .segments
        .iter()
        .filter(|s| {   // hidden -> нет субтитра; keep_original -> играет оригинал, субтитра нет (порт write_artifacts)
            !s.extra.get("hidden").and_then(|v| v.as_bool()).unwrap_or(false)
                && !s.extra.get("keep_original").and_then(|v| v.as_bool()).unwrap_or(false)
        })
        .map(|s| {
            let tgt = match overrides.get(s.id.as_str()) {
                Some(t) => t.to_string(),
                None => s.tgt_text.clone(),
            };
            (s, tgt)
        })
        .filter(|(_, tgt)| !tgt.trim().is_empty())
        .map(|(s, tgt)| {
            let end = if s.end > 0.0 { s.end } else { total };
            Sub {
                start: s.start,
                end,
                tgt,
                y: Some(seg_y(s.start, end)),
            }
        })
        .collect() };

    let preset = proj.captions.preset.name.clone();
    let caption_style = preset.as_deref().filter(|n| *n != "match");
    let args = dub_captions::BuildArgs {
        preset: caption_style,
        titles: &titles,
        subs: &subs,
        max_lines: 2,
        sub_y: Some(sub_y),
        sub_style: sub_style.as_ref(),
        caption_style,
        caption_plate: proj.captions.preset.plate.as_deref(),
        caption_reveal: proj.captions.preset.reveal.as_deref(),
        caption_font: proj.captions.preset.font.as_deref(),
        sub_px: proj
            .captions
            .raw_plan
            .get("sub_px")
            .and_then(|v| v.as_i64()),
    };
    dub_captions::build(vw, vh, out_ass, args)
}

/// Blur-боксы из Project (project.captions.blur_boxes, hidden исключаются). Порт caption_plan blur_boxes.
/// В режиме «без субтитров» (subs.mode=none) НЕ блюрим band-полосу (место оригинальных субтитров) — раз
/// мы не накладываем свои субтитры, незачем и закрашивать оригинал (баг-репорт: лишний блюр + прогон).
/// OCR-блюр экранного текста/титров (не-band) остаётся — локализация картинки от субтитров не зависит.
fn collect_blur_boxes(proj: &Project) -> Vec<BlurBox> {
    let drop_band = proj.subs.mode == "none";
    proj.captions
        .blur_boxes
        .iter()
        .filter(|b| !b.hidden)
        .filter(|b| !(drop_band && b.extra.get("band").and_then(|v| v.as_bool()).unwrap_or(false)))
        .map(|b| BlurBox { x: b.x, y: b.y, w: b.w, h: b.h, t0: b.t0, t1: b.t1, fill: b.fill.clone() })
        .collect()
}

fn map_title(t: &CoreTitle) -> CapTitle {
    CapTitle {
        text: if !t.tgt.is_empty() { t.tgt.clone() } else { t.text.clone() },
        bbox: t.bbox.clone(),
        color: t.color.clone(),
        bg: t.bg.clone(),
        font: t.font.clone(),
        italic: t.italic,
        align: t.align.clone(),
        start: t.start,
        end: t.end,
        lh: t.lh,
        solid: t.solid,
        bold: t.bold,
        size_px: t.size_px,
        outline: t.outline.clone(),
        outline_w: t.outline_w,
        shadow_dir: t.shadow_dir,
        uppercase: t.uppercase,
    }
}

fn map_sub_style(s: &CoreSubStyle) -> CapSubStyle {
    // background берём из extra (vision кладёт "background"); scene_* тоже могут быть в extra/типизированы.
    let bg = s
        .extra
        .get("background")
        .and_then(|v| v.as_str())
        .map(|x| x.to_string());
    let size_frac = s.extra.get("size_frac").and_then(|v| v.as_f64());
    CapSubStyle {
        color: s.color.clone(),
        background: bg,
        outline: Some(s.outline.clone()),
        outline_w: s.outline_w,
        shadow_dir: s.shadow_dir,
        bold: s.bold,
        italic: s.italic,
        uppercase: s.uppercase,
        align: s.align.clone(),
        font: s.font.clone(),
        n_lines: s.n_lines,
        size_frac,
        size_px: s.size_px,
        scene_color: s.scene_color.clone(),
        scene_flat: s.scene_flat,
        solid: s.extra.get("solid").and_then(|v| v.as_bool()).unwrap_or(false),
        // plate/plate_color из extra (PATCH caption их кладёт).
        plate: s.extra.get("plate").and_then(|v| v.as_bool()),
        plate_color: s
            .extra
            .get("plate_color")
            .and_then(|v| v.as_str())
            .map(|x| x.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dub_core::{BlurBox as CoreBlurBox, Segment};

    fn bb(x: i64, y: i64, w: i64, h: i64, t0: f64, t1: f64, band: bool) -> CoreBlurBox {
        let mut b = CoreBlurBox {
            x, y, w, h, t0, t1, hidden: false, fill: None, extra: Default::default(),
        };
        if band {
            b.extra.insert("band".into(), Value::Bool(true));
        }
        b
    }

    fn seg(id: &str, start: f64, end: f64, tgt: &str) -> Segment {
        Segment {
            id: id.into(),
            start,
            end,
            speaker: None,
            src_text: String::new(),
            tgt_text: tgt.into(),
            voice: None,
            dirty: false,
            extra: Default::default(),
        }
    }

    // Y самого раннего S-субтитра (наш дублированный) в ASS: \pos(cx,cy) -> cy.
    fn first_sub_y(ass: &str) -> i64 {
        for l in ass.lines() {
            if l.starts_with("Dialogue:") && l.contains(",S,") {
                if let Some(i) = l.find("\\pos(") {
                    let rest = &l[i + 5..];
                    let close = rest.find(')').unwrap();
                    let inner = &rest[..close];
                    let cy: i64 = inner.split(',').nth(1).unwrap().trim().parse().unwrap();
                    return cy;
                }
            }
        }
        panic!("нет S-субтитра с \\pos в ASS:\n{ass}");
    }

    fn build_to_string(proj: &Project, vw: i64, vh: i64) -> String {
        let dir = std::env::temp_dir()
            .join(format!("render_segy_{}_{}.ass", std::process::id(),
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        build_ass(proj, &dir, vw, vh, 44.77).unwrap();
        let s = std::fs::read_to_string(&dir).unwrap();
        let _ = std::fs::remove_file(&dir);
        s
    }

    // ducks_ru (ru→en), кадр s0 «SUBSCRIPTION.»: верхний title/tagline-блюр (cy≈438,464)
    // сидит в НИЖНЕЙ половине 824-кадра (>=0.45vh=371) вместе с настоящей полосой (cy≈630-642). Питон медианит
    // seg_y ТОЛЬКО по caption_boxes (полоса). Band-боксы помечены extra["band"], seg_y едет только
    // по ним -> строка на полосе, плашка (BorderStyle=3) обнимает текст там же, блюр оригинала накрыт.
    // Две фразы разной входной громкости после normalize_voice звучат одинаково (RMS сходится) —
    // все спикеры на выходе на одном уровне.
    #[test]
    fn normalize_equalizes_speaker_loudness() {
        let sr = 24000u32;
        let sine = |amp: f32| -> Vec<f32> {
            (0..sr) // 1 c
                .map(|i| amp * (2.0 * std::f64::consts::PI * 180.0 * i as f64 / sr as f64).sin() as f32)
                .collect()
        };
        let rms = |x: &[f32]| (x.iter().map(|&v| (v * v) as f64).sum::<f64>() / x.len() as f64).sqrt();
        let mut loud = sine(0.8);
        let mut quiet = sine(0.08);
        normalize_voice(&mut loud, sr);
        normalize_voice(&mut quiet, sr);
        let (rl, rq) = (rms(&loud), rms(&quiet));
        // после выравнивания уровни должны сойтись (dialog-gated нормализация); пики держит финальный
        // media::loudnorm true-peak лимитер на смиксованной дорожке (пофразного клэмпа нет — решение юзера).
        assert!((rl - rq).abs() / rl.max(1e-9) < 0.15, "уровни должны сойтись: loud={rl:.4} quiet={rq:.4}");
    }

    #[test]
    fn seg_y_rides_band_not_tagline_ducks() {
        let (vw, vh) = (464i64, 824i64);
        let mut proj = Project::default();
        proj.mode = "dub".into();
        proj.captions.sub_y = Some(634);
        proj.captions.sub_y_locked = false;
        // sub_style как у ducks: белый Oswald caps на тёмной полосе (vision background -> BorderStyle=3).
        let mut ss = CoreSubStyle::default();
        ss.color = "#FFFFFF".into();
        ss.bold = true;
        ss.uppercase = true;
        ss.font = Some("Oswald".into());
        ss.extra.insert("background".into(), Value::String("#000000".into()));
        proj.captions.sub_style = Some(ss);
        // Покадровый таглайн cy≈437/464 (10 боксов, НЕ band) равен по числу настоящей полосе
        // cy≈630-686 (9 боксов, band): без разделения списков таглайн затянул бы медиану.
        let mut boxes = Vec::new();
        for t in [1.75, 2.0, 2.25, 2.5, 2.75].iter() {
            boxes.push(bb(161, 427, 140, 21, *t, *t + 0.25, false)); // таглайн стр.1 cy=437
            boxes.push(bb(42, 453, 378, 22, *t, *t + 0.25, false));  // таглайн стр.2 cy=464 (широкая)
        }
        for (cy, t) in [(630.0, 1.75), (630.0, 2.0), (641.0, 2.25), (641.0, 2.5),
                        (641.0, 2.75), (641.0, 3.0), (652.0, 1.75), (652.0, 2.0), (686.0, 2.5)]
        {
            let y = cy as i64 - 9;
            boxes.push(bb(120, y, 220, 19, t, t + 0.25, true)); // полоса
        }
        proj.captions.blur_boxes = boxes;
        proj.segments = vec![
            seg("s0", 2.22, 2.80, "SUBSCRIPTION."), // окно 4-й word-группы s0
        ];
        let ass = build_to_string(&proj, vw, vh);
        let y = first_sub_y(&ass);
        assert!(
            (630..=650).contains(&y),
            "строка должна ехать на полосу оригинала (~640), а не на таглайн (~464): y={y}\n{ass}"
        );
        // плашка обнимает текст: S-style несёт BorderStyle=3 (плашка = обводка вокруг текста той же строки).
        let s_style = ass.lines().find(|l| l.starts_with("Style: S,")).unwrap();
        assert!(s_style.contains(",3,11,"), "плашка = BorderStyle=3 в стиле строки: {s_style}");
    }

    // Fallback: проект без маркеров band (ручной blur из редактора) — seg_y работает по всему
    // blur-набору. Здесь только полоса без тегов -> едет на неё.
    #[test]
    fn seg_y_fallback_untagged_boxes() {
        let (vw, vh) = (464i64, 824i64);
        let mut proj = Project::default();
        proj.mode = "dub".into();
        proj.captions.sub_y = Some(634);
        let mut boxes = Vec::new();
        for t in [0.75, 1.0, 1.25, 1.5].iter() {
            boxes.push(bb(120, 631, 220, 19, *t, *t + 0.25, false)); // НЕ помечены band
        }
        proj.captions.blur_boxes = boxes;
        proj.segments = vec![seg("s0", 1.0, 1.5, "TEXT")];
        let ass = build_to_string(&proj, vw, vh);
        let y = first_sub_y(&ass);
        assert!((630..=650).contains(&y), "fallback: без маркеров едем по всему набору: y={y}");
    }
}
