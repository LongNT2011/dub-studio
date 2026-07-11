//! DBNet-детекция текста (PP-OCR det.onnx). Препроцесс: resize к кратному 32, нормализация ImageNet;
//! постпроцесс: порог по вероятности -> связные компоненты (imageproc) -> bbox каждого региона,
//! отфильтрованные по площади. Возвращает боксы (x,y,w,h) в координатах ИСХОДНОГО кадра.
//!
//! Это упрощение классического DB-постпроцесса (без polygon unclip) — для нашей задачи (боксы под
//! блюр вшитых субтитров) достаточно bbox связной области: блюр всё равно растит бокс, а compose-слой
//! мержит строки. Никаких эвристик под конкретный клип — чистый порог+CC.

use crate::ort_engine::OnnxModel;
use image::{GrayImage, RgbImage};
use imageproc::region_labelling::{connected_components, Connectivity};
use ndarray::Array4;

/// Порог вероятности карты DB (стандарт PP-OCR 0.3).
const BOX_THRESH: f32 = 0.3;
/// Макс. сторона входа детектора (down-scale для скорости; PP-OCR дефолт 960).
const MAX_SIDE: u32 = 960;

/// Бокс детекции в координатах исходного кадра.
#[derive(Clone, Copy, Debug)]
pub struct DetBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Прогнать детектор на RGB-кадре -> список боксов. Порт det-части _lines (PP-OCR det).
pub fn detect(model: &mut OnnxModel, img: &RgbImage) -> Result<Vec<DetBox>, String> {
    let (ow, oh) = (img.width(), img.height());
    // resize: длинную сторону к <=MAX_SIDE, обе стороны кратны 32 (требование DBNet).
    let scale = (MAX_SIDE as f32 / ow.max(oh) as f32).min(1.0);
    let rw = (((ow as f32 * scale) / 32.0).round() as u32 * 32).max(32);
    let rh = (((oh as f32 * scale) / 32.0).round() as u32 * 32).max(32);
    let resized = image::imageops::resize(img, rw, rh, image::imageops::FilterType::Triangle);

    // NCHW f32, нормализация ImageNet (mean/std как в PP-OCR).
    let mean = [0.485f32, 0.456, 0.406];
    let std = [0.229f32, 0.224, 0.225];
    let mut input = Array4::<f32>::zeros((1, 3, rh as usize, rw as usize));
    for (x, y, px) in resized.enumerate_pixels() {
        for c in 0..3 {
            let v = px[c] as f32 / 255.0;
            input[[0, c, y as usize, x as usize]] = (v - mean[c]) / std[c];
        }
    }

    let (shape, data) = model.run(input)?;
    // выход [1,1,H,W] карта вероятностей.
    let (mh, mw) = (shape[shape.len() - 2], shape[shape.len() - 1]);
    // бинаризация -> GrayImage (255 где prob>thresh).
    let mut mask = GrayImage::new(mw as u32, mh as u32);
    for y in 0..mh {
        for x in 0..mw {
            let p = data[y * mw + x];
            mask.put_pixel(x as u32, y as u32, image::Luma([if p > BOX_THRESH { 255 } else { 0 }]));
        }
    }

    // связные компоненты -> bbox каждой метки.
    let labels = connected_components(&mask, Connectivity::Eight, image::Luma([0u8]));
    let mut bounds: std::collections::HashMap<u32, (u32, u32, u32, u32)> = std::collections::HashMap::new();
    for (x, y, px) in labels.enumerate_pixels() {
        let l = px[0];
        if l == 0 {
            continue;
        }
        let e = bounds.entry(l).or_insert((x, y, x, y));
        e.0 = e.0.min(x);
        e.1 = e.1.min(y);
        e.2 = e.2.max(x);
        e.3 = e.3.max(y);
    }

    // масштаб карты -> исходный кадр.
    let sx = ow as f32 / mw as f32;
    let sy = oh as f32 / mh as f32;
    let min_area = (mw * mh) as f32 * 0.00003; // отсечь пиксельный шум
    let mut boxes = Vec::new();
    for (_, (x0, y0, x1, y1)) in bounds {
        let bw = (x1 - x0 + 1) as f32;
        let bh = (y1 - y0 + 1) as f32;
        if bw * bh < min_area {
            continue;
        }
        boxes.push(DetBox {
            x: x0 as f32 * sx,
            y: y0 as f32 * sy,
            w: bw * sx,
            h: bh * sy,
        });
    }
    Ok(boxes)
}
