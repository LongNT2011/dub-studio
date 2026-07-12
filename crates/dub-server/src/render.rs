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
    pub fonts_dir: PathBuf,
    pub higgs_backend: String, // "cuda" | "cpu"
    pub higgs_device: i32,
    pub higgs_threads: i32,
    pub max_stretch: f64,
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

    let is_dub = proj.mode == "dub";
    let keep_music = proj.audio.keep_music;

    // ── АУДИО ──────────────────────────────────────────────────────────────────
    // Готовим финальную аудио-дорожку new_audio: dub (клон) поверх инструментала, либо оригинал.
    let new_audio: PathBuf = if is_dub {
        build_dub(proj, paths, total, keep_music, regen_dub, progress)?
    } else {
        // nodub/transcribe: оставляем оригинальную дорожку — mux возьмёт её из исходного видео.
        emit(progress, "mix", "nodub: оригинальная аудиодорожка");
        paths.input.clone()
    };

    // ── КАПШЕНЫ ────────────────────────────────────────────────────────────────
    emit(progress, "build", "сборка ASS (титры + дублированные субтитры)");
    let ass_path = wd.join("caps.ass");
    build_ass(proj, &ass_path, vw, vh, total)?;

    // ── BURN ───────────────────────────────────────────────────────────────────
    emit(progress, "burn", "вжигание субтитров + блюр (ffmpeg + libass, NVENC)");
    let blur_boxes = collect_blur_boxes(proj);
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

/// Полный аудио-конвейер дубляжа -> путь к new_audio. Порт _build_dub/_regen_dub (TTS+fit+timeline+mix).
fn build_dub(
    proj: &Project,
    paths: &RenderPaths,
    total: f64,
    keep_music: bool,
    regen_dub: bool,
    progress: &Progress,
) -> Result<PathBuf, String> {
    let wd = &paths.work_dir;
    // Сегменты с непустым tgt (как в питоне: только строки с текстом синтезируются). Несём индекс в
    // ПОЛНОМ списке proj.segments — слот next.start считается по индексу i+1 полного списка (порт
    // pipeline.py:207: nxt = segs[i+1].start, где segs — весь транскрипт, пустые пропускаются continue,
    // но индекс i+1 идёт по полному списку).
    let segs: Vec<(usize, &dub_core::Segment)> = proj
        .segments
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.tgt_text.trim().is_empty())
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
    let (vocals, instrumental): (PathBuf, Option<PathBuf>) = if keep_music {
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

    // 3) клон-референс НА КАЖДОГО спикера: длиннейшая реплика спикера -> ref_spk{N}.wav (voices.resolve
    //    clone). Мультиспикер-ролик озвучивается голосом своего диаризованного спикера.
    let spk_refs = build_speaker_refs(&segs, &vocals16, wd)?;
    let first_ref = spk_refs.values().next().cloned().unwrap_or_else(|| wd.join("ref.wav"));
    let ref_of = |s: &dub_core::Segment| -> PathBuf {
        s.speaker
            .as_deref()
            .and_then(|k| spk_refs.get(k))
            .cloned()
            .unwrap_or_else(|| first_ref.clone())
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
            Some("q8_0"),
        )
        .map_err(|e| format!("Higgs load_model: {e}"))?;

    // placed = [(at, wav_path)]. cursor-aware fit (как в питоне). Имя seg-файла и слот next.start —
    // по индексу fi в ПОЛНОМ списке proj.segments (питон: seg_{i:03d}.wav, nxt = segs[i+1].start).
    let mut placed: Vec<(f64, PathBuf)> = Vec::with_capacity(segs.len());
    let mut cursor = 0.0f64;
    let n_all = proj.segments.len();
    for &(fi, s) in segs.iter() {
        let tgt = s.tgt_text.trim();
        let raw = wd.join(format!("seg_{:03}.wav", fi));
        let ref_wav = ref_of(s);
        // синтез: dirty ИЛИ нет кэша ИЛИ кэш старше рефа спикера (реф пересобран -> голос сменился).
        let stale_ref = raw
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .zip(ref_wav.metadata().and_then(|m| m.modified()).ok())
            .map(|(seg_t, ref_t)| seg_t < ref_t)
            .unwrap_or(true);
        let need_synth = (regen_dub && s.dirty) || !raw.is_file() || stale_ref;
        if need_synth {
            let (samples, sr) = engine
                .voice_clone(tgt, &ref_wav.to_string_lossy(), None, "")
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

    // 6) свести с инструменталом (если есть).
    if let Some(inst) = instrumental {
        emit(progress, "mix", "сведение: инструментал + дубль-вокал");
        let new_audio = wd.join("new_audio.m4a");
        media::mix(&dub, &inst, &new_audio)?;
        Ok(new_audio)
    } else {
        Ok(dub)
    }
}

/// Референс клона на КАЖДОГО спикера: {speaker -> ref_spk{N}.wav} из его длиннейшей реплики (<=12с).
/// Порт voices.resolve clone-ветки. Спикер None -> ключ "0" (моно-ролик = один реф).
fn build_speaker_refs(
    segs: &[(usize, &dub_core::Segment)],
    vocals16: &Path,
    wd: &Path,
) -> Result<std::collections::BTreeMap<String, PathBuf>, String> {
    let mut refs: std::collections::BTreeMap<String, PathBuf> = std::collections::BTreeMap::new();
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
        media::trim(vocals16, &ref_wav, cand.start, cand.end.min(cand.start + 12.0))?;
        refs.insert(spk, ref_wav);
    }
    Ok(refs)
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
        let (s, ssr) = if *wav == placed[0].1 {
            (first.0.clone(), first.1)
        } else {
            wavio::read_mono_f32(wav)?
        };
        let _ = ssr;
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
    build_ass(proj, out_ass, vw, vh, total)?;
    let blur_boxes = collect_blur_boxes(proj);
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

/// Собрать ASS через dub-captions из Project. Порт captions.build call-site pipeline.run.
pub(crate) fn build_ass(proj: &Project, out_ass: &Path, vw: i64, vh: i64, total: f64) -> Result<(), String> {
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
    let subs: Vec<Sub> = proj
        .segments
        .iter()
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
        .collect();

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
fn collect_blur_boxes(proj: &Project) -> Vec<BlurBox> {
    proj.captions
        .blur_boxes
        .iter()
        .filter(|b| !b.hidden)
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
