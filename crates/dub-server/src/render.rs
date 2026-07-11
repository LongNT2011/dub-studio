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
    // Сегменты с непустым tgt (как в питоне: только строки с текстом синтезируются).
    let segs: Vec<&dub_core::Segment> =
        proj.segments.iter().filter(|s| !s.tgt_text.trim().is_empty()).collect();
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

    // 3) клон-референс: самый длинный сегмент с текстом (voices.resolve clone-ветка, x_vector_only).
    //    Higgs-клон использует РЕФ-WAV; ref_text опускаем (x-vector, чтоб исходный язык не блидил акцент).
    let ref_wav = pick_reference(&segs, &vocals16, wd, total)?;

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

    // placed = [(at, wav_path)]. cursor-aware fit (как в питоне).
    let mut placed: Vec<(f64, PathBuf)> = Vec::with_capacity(segs.len());
    let mut cursor = 0.0f64;
    // индекс сегмента в общем массиве для «следующего по времени» слота.
    let all: Vec<&dub_core::Segment> = proj.segments.iter().collect();
    for (i, s) in segs.iter().enumerate() {
        let tgt = s.tgt_text.trim();
        let raw = wd.join(format!("seg_{:03}.wav", i));
        // регенерация: синтез если dirty ИЛИ кэша нет ИЛИ полный regen выключен но файла нет.
        let need_synth = regen_dub && s.dirty || !raw.is_file();
        if need_synth {
            let (samples, sr) = engine
                .voice_clone(tgt, &ref_wav.to_string_lossy(), None, "")
                .map_err(|e| format!("Higgs clone seg{i}: {e}"))?;
            let wav = AudiocppEngine::encode_wav(&samples, sr, 1);
            std::fs::write(&raw, &wav).map_err(|e| format!("запись seg{i}: {e}"))?;
        }
        // слот: от текущего onset до следующей строки ПО ВРЕМЕНИ / конца видео.
        let at = s.start.max(cursor);
        let nxt = next_start_after(&all, s.start, total);
        let room = (nxt - at).max(0.3);
        let fit = fit_to_slot(&raw, room, &wd.join(format!("seg_{:03}_fit.wav", i)), paths.max_stretch)?;
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

/// Референс клона: самый длинный сегмент с текстом -> trim из вокала (<=12с). Порт _pick_reference.
fn pick_reference(
    segs: &[&dub_core::Segment],
    vocals16: &Path,
    wd: &Path,
    _total: f64,
) -> Result<PathBuf, String> {
    let cand = segs
        .iter()
        .max_by(|a, b| (a.end - a.start).partial_cmp(&(b.end - b.start)).unwrap())
        .unwrap();
    let ref_wav = wd.join("ref.wav");
    media::trim(vocals16, &ref_wav, cand.start, cand.end.min(cand.start + 12.0))?;
    Ok(ref_wav)
}

/// Следующий по ВРЕМЕНИ старт сегмента после `start` (или total). Аналог segs[i+1].start в питоне,
/// где segs отсортированы по времени.
fn next_start_after(all: &[&dub_core::Segment], start: f64, total: f64) -> f64 {
    all.iter()
        .map(|s| s.start)
        .filter(|&st| st > start + 1e-6)
        .fold(f64::INFINITY, f64::min)
        .min(total)
        .max(if total.is_finite() { 0.0 } else { total })
        .min(total)
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

/// Собрать ASS через dub-captions из Project. Порт captions.build call-site pipeline.run.
fn build_ass(proj: &Project, out_ass: &Path, vw: i64, vh: i64, total: f64) -> Result<(), String> {
    let titles: Vec<CapTitle> = proj.captions.titles.iter().map(map_title).collect();
    let sub_style = proj.captions.sub_style.as_ref().map(map_sub_style);
    // sub_y: per-segment y (пока — общий sub_y; per-line ride оригинальной полосы делает OCR-стадия
    // через caption_boxes, которых в раунде до OCR нет). Дефолт vh*0.82 если не задан.
    let sub_y = proj.captions.sub_y.unwrap_or((vh as f64 * 0.82) as i64);
    let subs: Vec<Sub> = proj
        .segments
        .iter()
        .filter(|s| !s.tgt_text.trim().is_empty())
        .map(|s| Sub {
            start: s.start,
            end: if s.end > 0.0 { s.end } else { total },
            tgt: s.tgt_text.clone(),
            y: Some(sub_y),
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
        .map(|b| BlurBox { x: b.x, y: b.y, w: b.w, h: b.h, t0: b.t0, t1: b.t1 })
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
    }
}
