//! preview_frame / source_frame (порт api.preview_frame / api.source_frame). CPU-дёшево, БЕЗ моделей:
//! preview заново собирает .ass из ТЕКУЩЕГО (отредактированного) Project и композитит ОДИН кадр
//! (blur-боксы + ASS-оверлей) через ffmpeg input-seek — тот же build_ass, что и полный рендер (WYSIWYG-
//! паритет), плюс burn_frame из dub-captions. source_frame — сырой кадр оригинала (для before/after).
//!
//! Обе функции синхронные и тяжеловаты только на ffmpeg — гоняются через GPU-воркер (как в app.py:
//! serialized preview_frame), возвращают PNG-байты.

use dub_core::Project;
use std::path::Path;

use crate::render::build_ass;

/// Собрать ОДИН превью-кадр (PNG-байты) на времени t. Порт api.preview_frame: build ASS из текущего
/// Project + burn_frame (blur-боксы + ASS). fonts_dir нужен libass/измерению глифов.
pub fn preview_frame(
    proj: &Project,
    input: &Path,
    work_dir: &Path,
    fonts_dir: &Path,
    t: f64,
) -> Result<Vec<u8>, String> {
    dub_captions::set_fonts_dir(fonts_dir);
    let vw = proj.meta.width;
    let vh = proj.meta.height;
    let total = proj.meta.duration;
    let ass_p = work_dir.join("_preview.ass");
    build_ass(proj, &ass_p, vw, vh, total)?;

    // blur-боксы = project.captions.blur_boxes (hidden исключаются) — как collect_blur_boxes рендера.
    let blur_boxes: Vec<dub_captions::BlurBox> = proj
        .captions
        .blur_boxes
        .iter()
        .filter(|b| !b.hidden)
        .map(|b| dub_captions::BlurBox {
            x: b.x,
            y: b.y,
            w: b.w,
            h: b.h,
            t0: b.t0,
            t1: b.t1,
        })
        .collect();

    let png = work_dir.join("_preview.png");
    dub_captions::burn_frame(
        input,
        &ass_p,
        &png,
        t,
        &blur_boxes,
        Some((vw, vh)),
        proj.render.blur,
        proj.render.blur_sigma,
    )?;
    std::fs::read(&png).map_err(|e| format!("чтение превью-кадра: {e}"))
}

/// Сырой кадр ОРИГИНАЛА (PNG-байты) на t — без сабов/блюра/дубляжа (для before/after). Порт
/// api.source_frame: ffmpeg input-seek extract одного кадра, без моделей.
pub fn source_frame(input: &Path, work_dir: &Path, t: f64) -> Result<Vec<u8>, String> {
    #[cfg(windows)]
    const FFMPEG: &str = "ffmpeg.exe";
    #[cfg(not(windows))]
    const FFMPEG: &str = "ffmpeg";
    let png = work_dir.join("_original.png");
    let out = std::process::Command::new(FFMPEG)
        .arg("-y")
        .arg("-ss")
        .arg(format!("{:.2}", t.max(0.0)))
        .arg("-i")
        .arg(input)
        .args(["-frames:v", "1", "-update", "1"])
        .arg(&png)
        .output()
        .map_err(|e| format!("ffmpeg запуск: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: String = err.chars().rev().take(600).collect::<String>().chars().rev().collect();
        return Err(format!("ffmpeg source_frame rc={:?}: {tail}", out.status.code()));
    }
    std::fs::read(&png).map_err(|e| format!("чтение кадра оригинала: {e}"))
}
