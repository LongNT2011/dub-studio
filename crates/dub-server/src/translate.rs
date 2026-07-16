//! Стадия перевода + vision для analyze (раунд 3). Порт pipeline._build_dub translate-ветки поверх
//! крейта dub-translate (Gemma через сайдкар llama-server). Заполняет tgt_text сегментов, titles/brands/
//! sub_style/sub_y и raw_ctx в Project. Все решения (do_translate / same_lang / rewrite) — как в питоне.
//!
//! Fail-safe: любой сбой (нет llama-бинаря / нет весов / упал сервер) логируется в SSE и оставляет tgt
//! пустым — перевод не блокирует транскрипт-стадию analyze (её результат уже валиден).

use dub_core::{Brand, Project, SubStyle};
use dub_llm::{ChatClient, LlamaServer, ServerOpts};
use dub_translate::{ctx_run, CtxConfig, Seg};
use serde_json::Value;

use crate::analyze::{AnalyzeArgs, AnalyzePaths, Progress};

fn emit(progress: &Progress, stage: &str, msg: &str) {
    progress(serde_json::json!({ "stage": stage, "msg": msg }));
}

/// tgt = исходный текст (без MT) — для transcribe- и same-lang-веток.
fn copy_src_to_tgt(proj: &mut Project) {
    for s in &mut proj.segments {
        s.tgt_text = s.src_text.clone();
    }
}

/// Нужно ли переводить: dub/voiceover-режим ИЛИ subs=translate (как do_translate в pipeline).
fn wants_translate(proj: &Project) -> bool {
    proj.mode == "dub" || proj.mode == "voiceover" || proj.subs.mode == "translate"
}

/// Прогнать стадию. proj уже собран транскрипт-стадией (segments + mode/tgt_lang). vh/total — из probe.
pub fn stage(
    args: &AnalyzeArgs,
    paths: &AnalyzePaths,
    proj: &mut Project,
    vocals16: &std::path::Path,
    vh: i64,
    total: f64,
    progress: &Progress,
) {
    // Нет сегментов -> нечего переводить (auto-nodub / музыка). Как ранний return в питоне.
    if proj.segments.is_empty() {
        return;
    }
    // Импортированы субтитры УЖЕ на языке перевода: tgt заполнен из cues (analyze import-ветка),
    // MT и vision-раскладка не нужны — Даб Студио только озвучивает готовый текст.
    if args.import_translated {
        emit(progress, "translate", "субтитры уже на языке перевода -> без MT (только озвучка)");
        return;
    }
    let rewrite = if args.rewrite.is_empty() { None } else { Some(args.rewrite.as_str()) };
    let do_translate = wants_translate(proj) || rewrite.is_some();
    if !do_translate {
        // transcribe-режим: tgt = исходный текст, БЕЗ MT (parity с pipeline «transcribe» веткой).
        copy_src_to_tgt(proj);
        emit(progress, "translate", "transcribe: tgt=исходный текст, без перевода");
        return;
    }

    // src == tgt -> оставить исходник, ноль MT (same_lang в питоне). src берём из query (auto -> не знаем
    // язык детерминированно тут; ASR его не вернул типизированно, потому same_lang проверяем лишь по
    // явному src_lang — как str(src).lower()==tgt в питоне при известном src).
    let src = &args.src_lang;
    let src_lc = src.to_lowercase();
    let same_lang = !src.is_empty() && src_lc != "auto" && src_lc == proj.tgt_lang.to_lowercase();
    if same_lang && rewrite.is_none() {
        copy_src_to_tgt(proj);
        emit(progress, "translate", "same-lang -> без MT (tgt=исходник)");
        return;
    }

    // Поднять сайдкар Gemma (+mmproj для vision). Существование бинаря/весов проверяет start().
    if !paths.llama_bin.is_file() {
        emit(progress, "translate", &format!(
            "перевод пропущен: llama-server не найден ({})", paths.llama_bin.display()));
        return;
    }
    if !paths.mt_model.is_file() {
        emit(progress, "translate", &format!(
            "перевод пропущен: GGUF Gemma не найден ({})", paths.mt_model.display()));
        return;
    }

    emit(progress, "translate", "поднимаю llama-server (Gemma + mmproj)");
    // Лимит prefill-батча из настройки «Экономия RAM» (active.json.llama_ubatch) — против OOM на слабой RAM.
    let mut opts = ServerOpts::new(&paths.llama_bin, &paths.mt_model)
        .with_ubatch(crate::models::sel_num(&paths.models_root, "llama_ubatch").map(|f| f as u32));
    if paths.mmproj.is_file() {
        opts = opts.with_mmproj(&paths.mmproj);
    }
    let srv = match LlamaServer::start(&opts) {
        Ok(s) => s,
        Err(e) => {
            emit(progress, "translate", &format!("llama-server не поднялся: {e}; перевод пропущен"));
            return;
        }
    };
    let client = match ChatClient::new(srv.base_url()) {
        Ok(c) => c,
        Err(e) => {
            emit(progress, "translate", &format!("клиент чата: {e}; перевод пропущен"));
            return;
        }
    };

    // Seg-вью для dub-translate (text/speaker). speaker -> i64 (питон speaker=0 по умолчанию).
    let mut segs: Vec<Seg> = proj
        .segments
        .iter()
        .map(|s| {
            let spk = s.speaker.as_deref().and_then(|x| x.parse::<i64>().ok()).unwrap_or(0);
            let mut seg = Seg::new(s.src_text.clone(), spk);
            seg.start = s.start;
            seg.end = s.end;
            seg
        })
        .collect();

    let cfg = CtxConfig {
        input: paths.input.clone(),
        work_dir: paths.work_dir.clone(),
        tgt_lang: proj.tgt_lang.clone(),
        vocals16: if vocals16.is_file() { Some(vocals16.to_path_buf()) } else { None },
        vh: vh as f64,
        total,
    };

    emit(progress, "vision", "ctx-проход: vision layout/scene + перевод транскрипта");
    let res = ctx_run(&client, &cfg, &mut segs, rewrite, |m| {
        emit(progress, "vision", m);
    });

    // Сервер больше не нужен -> глушим (освобождаем VRAM, как del llm в питоне перед TTS/берном).
    drop(srv);

    let extra = match res {
        Ok(r) => r.extra,
        Err(e) => {
            emit(progress, "translate", &format!("ctx-перевод не удался: {e}; tgt оставлен пустым"));
            return;
        }
    };

    // Перенести tgt в сегменты Project. segs строился 1:1 из proj.segments и дальше не используется —
    // переносим строки перемещением (zip по равной длине, без клонов).
    for (s, sg) in proj.segments.iter_mut().zip(segs) {
        s.tgt_text = sg.tgt;
    }

    // Замапить extra -> типизированные поля Project + сохранить сырой ctx (byte-identical passthrough,
    // как raw_ctx = ce_d в from_artifacts).
    apply_extra(proj, &extra);

    let translated = proj.segments.iter().filter(|s| !s.tgt_text.is_empty()).count();
    emit(progress, "translate", &format!(
        "перевод готов: {}/{} строк, тайтлов={}",
        translated, proj.segments.len(), proj.captions.titles.len()));
}

/// extra (ctx_extra.json) -> типизированные captions.sub_style/sub_y/titles/brands + raw_ctx.
/// Точная параллель project.from_artifacts (строки 239-249): raw_ctx = сырой extra; sub_style/titles/
/// brands десериализуются в типы (extra="allow" ловит все ключи vision-словаря).
fn apply_extra(proj: &mut Project, extra: &Value) {
    // raw_ctx — весь ctx как есть (для будущего byte-identical re-render captions-стадии раунда 4).
    if let Value::Object(m) = extra {
        proj.raw_ctx = m.clone();
    }
    // sub_style
    if let Some(ss) = extra.get("sub_style") {
        if ss.is_object() {
            if let Ok(style) = serde_json::from_value::<SubStyle>(ss.clone()) {
                proj.captions.sub_style = Some(style);
            }
        }
    }
    // sub_y
    if let Some(y) = extra.get("sub_y").and_then(|v| v.as_i64()) {
        proj.captions.sub_y = Some(y);
    }
    // titles: НЕ строим здесь. Их финальный вид (bbox + время + стиль) собирает caption-композит
    // (compose.rs) ПОСЛЕ OCR-стадии — до OCR у нас нет localize-боксов для матчинга y_frac -> bbox, а
    // без bbox emit_title молча скипает титр. raw_ctx["titles"] (уже с tgt) переносится выше как есть,
    // композит читает его оттуда. Порт pipeline.run:497-543 живёт в compose::run.
    // brands
    if let Some(arr) = extra.get("brands").and_then(|v| v.as_array()) {
        proj.captions.brands = arr
            .iter()
            .filter(|b| b.is_object())
            .filter_map(|b| serde_json::from_value::<Brand>(b.clone()).ok())
            .collect();
    }
}

