//! OCR-стадия analyze (раунд 4). Порт pipeline.run ocr_detect + compose.analyze_layout: детектим
//! вшитый текст (dub-ocr), выделяем субтитр-полосу -> блюр-боксы, уточняем sub_y. Спикер-словарь
//! (spoken) из транскрипта отсекает сцен-графику (не-сказанный текст) от субтитр-полосы.
//!
//! Fail-safe: любой сбой (нет моделей OCR / нет ORT_DYLIB_PATH / ffmpeg) логируется в SSE и оставляет
//! blur_boxes пустыми — analyze не падает (блюр не блокер, редактор добавит руками).

use dub_core::{BlurBox, Project};
use dub_ocr::{analyze_layout, detect_regions, OcrPaths};
use std::collections::HashSet;

use crate::analyze::{AnalyzePaths, Progress};

fn emit(progress: &Progress, stage: &str, msg: &str) {
    progress(serde_json::json!({ "stage": stage, "msg": msg }));
}

/// Прогнать OCR-стадию. Заполняет proj.captions.blur_boxes (субтитр-полоса) и уточняет sub_y.
pub fn stage(paths: &AnalyzePaths, proj: &mut Project, vh: i64, progress: &Progress) {
    let ocr_paths = OcrPaths::under(&paths.models_root);
    if !ocr_paths.all_exist() {
        emit(progress, "ocr_detect", "модели OCR не найдены -> без блюр-боксов");
        return;
    }
    emit(progress, "ocr_detect", "детекция вшитого текста (PP-OCR DBNet+CRNN)");

    // spoken vocab из исходного транскрипта (для отсечения сцен-графики от субтитр-полосы).
    let spoken: HashSet<String> = proj
        .segments
        .iter()
        .flat_map(|s| {
            s.src_text
                .split(|c: char| !c.is_alphabetic())
                .filter(|w| !w.is_empty())
                .map(|w| w.to_lowercase())
        })
        .collect();

    // detect_regions: fps=caption_fps, дефолты как в питоне (min_dur .3, iou .3, pad 8, jitter 20, score .4).
    let fps = paths.caption_fps.max(1);
    let (regions, raw) = match detect_regions(
        &paths.input,
        &paths.work_dir,
        &ocr_paths,
        fps,
        0.3,
        0.3,
        8,
        20.0,
        0.4,
    ) {
        Ok(r) => r,
        Err(e) => {
            emit(progress, "ocr_detect", &format!("OCR-детекция не удалась ({e}); без блюра"));
            return;
        }
    };

    let (_localize, caption_boxes, sub_y_det) = analyze_layout(&regions, vh, &raw, &spoken);

    // caption_boxes -> blur_boxes Project (субтитр-полоса под блюр). Порт band_blur/blur_boxes.
    // Каждый бокс с небольшим ростом (как питон: +6/-4 по x, +8/-4 по y округлённо — тут держим как
    // есть, burn сам растит на 2px и pre-roll). t1 = t0 + окно семпла (1/fps) для frame-accurate блюра.
    let dt = 1.0 / fps as f64;
    let mut blur: Vec<BlurBox> = Vec::new();
    for (x, y, w, h, t0, t1) in &caption_boxes {
        let t1e = if (*t1 as f64) > (*t0 as f64) { *t1 as f64 } else { *t0 as f64 + dt };
        blur.push(BlurBox {
            x: (*x - 6).max(0),
            y: (*y - 4).max(0),
            w: *w + 12,
            h: *h + 8,
            t0: (*t0 as f64 * 100.0).round() / 100.0,
            t1: (t1e * 100.0).round() / 100.0,
            hidden: false,
            extra: Default::default(),
        });
    }
    // объединять перекрывающиеся боксы на одном месте в один спан не обязательно — burn переиспользует
    // ОДИН full-frame gblur и композитит по каждому боксу; лишние боксы лишь чуть удлиняют цепочку.
    let n = blur.len();
    proj.captions.blur_boxes = blur;

    // sub_y: OCR-детект (усреднён по всем кадрам) — источник истины позиции; перекрывает vision-fallback,
    // если он был. Не трогаем, если editor уже зафиксировал (sub_y_locked).
    if !proj.captions.sub_y_locked {
        if let Some(y) = sub_y_det {
            proj.captions.sub_y = Some(y);
        }
    }

    emit(
        progress,
        "ocr_detect",
        &format!("OCR: {} регионов -> {} блюр-боксов, sub_y={:?}", regions.len(), n, sub_y_det),
    );
}
