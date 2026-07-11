//! CRNN-распознавание (PP-OCR rec_cyrillic.onnx) + CTC-декод. Кроп бокса -> resize к высоте 48
//! (ширина пропорц., <=макс), нормализация [-1,1], инференс -> [1,T,C] -> greedy CTC по dict.
//!
//! dict PP-OCR: индекс 0 = CTC blank; символы из .dict.txt идут с индекса 1; PP-OCR добавляет пробел
//! последним классом. Итог: C = len(dict)+2 (blank + dict + space). Совпадает с 165 = 163+2.

use crate::ort_engine::OnnxModel;
use image::RgbImage;
use ndarray::Array4;

/// Высота входа rec (стандарт PP-OCR).
const REC_H: u32 = 48;
/// Макс. ширина одного кропа (широкие боксы бьём? — нет, ресайзим; кап от переполнения памяти).
const REC_MAX_W: u32 = 1600;

/// Словарь распознавания: строки из .dict.txt. class 0 = blank; 1..=n = dict[i-1]; n+1 = space.
pub struct RecDict {
    chars: Vec<String>,
}

impl RecDict {
    /// Загрузить dict-файл (по строке на символ). Порядок = порядок классов модели (с индекса 1).
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let txt = std::fs::read_to_string(path).map_err(|e| format!("dict {}: {e}", path.display()))?;
        // НЕ trim строк целиком — первая строка PP-OCR часто пробел (значимый символ). Убираем лишь \r\n.
        let chars: Vec<String> = txt
            .split('\n')
            .map(|l| l.trim_end_matches('\r').to_string())
            .collect();
        // последняя пустая строка от финального \n -> отбрасываем, но пробел-класс добавит decode.
        let chars = if chars.last().map(|s| s.is_empty()).unwrap_or(false) {
            chars[..chars.len() - 1].to_vec()
        } else {
            chars
        };
        Ok(Self { chars })
    }

    /// class index -> символ. 0=blank(""), 1..=n=dict, n+1=space.
    fn ch(&self, idx: usize) -> &str {
        if idx == 0 {
            ""
        } else if idx <= self.chars.len() {
            &self.chars[idx - 1]
        } else {
            " "
        }
    }
}

/// Распознать текст в кропе бокса. Порт rec-части _lines (PP-OCR rec + CTC greedy).
pub fn recognize(
    model: &mut OnnxModel,
    dict: &RecDict,
    img: &RgbImage,
) -> Result<(String, f32), String> {
    if img.width() == 0 || img.height() == 0 {
        return Ok((String::new(), 0.0));
    }
    // resize к высоте 48, ширина пропорционально.
    let rw = ((img.width() as f32 * REC_H as f32 / img.height() as f32).round() as u32)
        .clamp(8, REC_MAX_W);
    // Lanczos3 ближе к PIL bicubic, что использует питон-референс rec -> меньше шума в CTC.
    let resized = image::imageops::resize(img, rw, REC_H, image::imageops::FilterType::Lanczos3);

    // NCHW, нормализация [-1,1]: (x/255 - 0.5)/0.5.
    let mut input = Array4::<f32>::zeros((1, 3, REC_H as usize, rw as usize));
    for (x, y, px) in resized.enumerate_pixels() {
        for c in 0..3 {
            input[[0, c, y as usize, x as usize]] = (px[c] as f32 / 255.0 - 0.5) / 0.5;
        }
    }

    let (shape, data) = model.run(input)?;
    // выход [1,T,C].
    let (t, c) = (shape[shape.len() - 2], shape[shape.len() - 1]);
    // greedy CTC: argmax по C на каждом t, схлопнуть повторы, убрать blank(0).
    let mut out = String::new();
    let mut scores = Vec::new();
    let mut prev = usize::MAX;
    for ti in 0..t {
        let row = &data[ti * c..(ti + 1) * c];
        let (mut best, mut bestv) = (0usize, f32::NEG_INFINITY);
        for (i, &v) in row.iter().enumerate() {
            if v > bestv {
                bestv = v;
                best = i;
            }
        }
        if best != 0 && best != prev {
            out.push_str(dict.ch(best));
            scores.push(bestv);
        }
        prev = best;
    }
    let conf = if scores.is_empty() {
        0.0
    } else {
        scores.iter().sum::<f32>() / scores.len() as f32
    };
    Ok((out, conf))
}
