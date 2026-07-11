//! Распознавание текста (PP-OCR CRNN + CTC). Дефолты config.yaml: rec_img_shape=[3,48,320].
//! Словарь символов берётся из метаданных ONNX (ключ "character") — как RapidOCR v3
//! (get_character_list). CTC-декод: argmax по кадрам, схлопывание повторов, пропуск blank(0).
//!
//! Словарь ONNX (ключ "character") УЖЕ содержит полный набор символов PaddleOCR (для cyrillic-модели:
//! space ПЕРВЫМ токеном, пустой токен ПОСЛЕДНИМ); его длина + 1(blank) == размер выхода softmax (C=165).
//! Поэтому НЕ добавляем/не переписываем токены — только префиксуем blank в позицию 0. Любая правка
//! (доп. пробел в конце / замена пустого токена) сдвигает индексы и портит CTC-декод — это и был
//! корневой шум («КОРОЧЕ» -> «kрhеh»). Fallback на .dict.txt даёт то же: blank + строки файла как есть.

use crate::ort_engine::OnnxModel;
use image::RgbImage;
use ndarray::Array4;

/// Высота входа rec (стандарт PP-OCR).
const REC_H: usize = 48;

/// Словарь распознавания: blank в позиции 0, далее символы из метаданных ONNX (или .dict.txt).
pub struct RecDict {
    chars: Vec<String>, // chars[0] = blank
}

impl RecDict {
    /// Прочитать словарь из метаданных сессии (ключ "character") — источник истины RapidOCR v3.
    pub fn from_session_meta(model: &OnnxModel) -> Result<Self, String> {
        let raw = model
            .metadata_character()?
            .ok_or_else(|| "в модели нет метаданных 'character'".to_string())?;
        Ok(Self::from_dict_str(&raw))
    }

    /// Прочитать словарь из текста метаданных (по одному токену на строку). Токены берутся КАК ЕСТЬ,
    /// blank префиксуется в позицию 0. Хвостовой пустой токен НЕ отбрасывается — он значим (класс C-1).
    pub fn from_dict_str(raw: &str) -> Self {
        let list: Vec<String> = raw.split('\n').map(|s| s.to_string()).collect();
        let mut chars = Vec::with_capacity(list.len() + 1);
        chars.push(String::new()); // blank index 0
        chars.extend(list);
        Self { chars }
    }

    /// Fallback: загрузить словарь из .dict.txt (по строке на символ). blank префиксуется. Убираем лишь
    /// \r; порядок = порядок классов модели с индекса 1. Предпочтителен путь from_session_meta.
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let txt = std::fs::read_to_string(path).map_err(|e| format!("dict {}: {e}", path.display()))?;
        // нормализуем перевод строк, не трогая содержимое токенов (первая строка часто пробел).
        let normalized = txt.replace("\r\n", "\n").replace('\r', "");
        // отбрасываем единственный хвостовой перевод строки от финального \n (но не значимый пустой
        // токен внутри метаданных — тут файл, где хвостовой \n — просто конец файла).
        let normalized = normalized.strip_suffix('\n').unwrap_or(&normalized);
        Ok(Self::from_dict_str(normalized))
    }

    /// class index -> символ. 0=blank(""), далее токены словаря.
    fn ch(&self, idx: usize) -> &str {
        self.chars.get(idx).map(|s| s.as_str()).unwrap_or("")
    }
}

/// Распознать текст в кропе бокса. Порт rec-части _lines (PP-OCR rec + CTC greedy). Билинейный ресайз
/// к высоте 48 (ширина пропорц.), нормализация [-1,1].
pub fn recognize(model: &mut OnnxModel, dict: &RecDict, img: &RgbImage) -> Result<(String, f32), String> {
    let (cw, ch) = (img.width() as usize, img.height() as usize);
    if cw == 0 || ch == 0 {
        return Ok((String::new(), 0.0));
    }
    // ширина пропорционально высоте 48, ограничение снизу разумным минимумом.
    let ratio = cw as f32 / ch as f32;
    let rw = ((REC_H as f32 * ratio).ceil() as usize).max(16);
    let mut input = Array4::<f32>::zeros((1, 3, REC_H, rw));
    let sx = cw as f32 / rw as f32;
    let sy = ch as f32 / REC_H as f32;
    for oy in 0..REC_H {
        let fy = ((oy as f32 + 0.5) * sy - 0.5).clamp(0.0, ch as f32 - 1.0);
        let y0 = fy.floor() as usize;
        let y1 = (y0 + 1).min(ch - 1);
        let wy = fy - y0 as f32;
        for ox in 0..rw {
            let fx = ((ox as f32 + 0.5) * sx - 0.5).clamp(0.0, cw as f32 - 1.0);
            let x0 = fx.floor() as usize;
            let x1 = (x0 + 1).min(cw - 1);
            let wx = fx - x0 as f32;
            for c in 0..3 {
                let s = |xx: usize, yy: usize| img.get_pixel(xx as u32, yy as u32)[c] as f32;
                let top = s(x0, y0) * (1.0 - wx) + s(x1, y0) * wx;
                let bot = s(x0, y1) * (1.0 - wx) + s(x1, y1) * wx;
                let v = (top * (1.0 - wy) + bot * wy) / 255.0;
                input[[0, c, oy, ox]] = (v - 0.5) / 0.5;
            }
        }
    }

    let (shape, flat) = model.run(input)?;
    // форма (1, T, C): C = размер словаря.
    if shape.len() != 3 {
        return Ok((String::new(), 0.0));
    }
    let t = shape[1];
    let c = shape[2];

    let mut text = String::new();
    let mut scores: Vec<f32> = Vec::new();
    let mut prev_idx = usize::MAX;
    for ti in 0..t {
        let base = ti * c;
        let mut best = 0usize;
        let mut best_v = f32::NEG_INFINITY;
        for ci in 0..c {
            let v = flat[base + ci];
            if v > best_v {
                best_v = v;
                best = ci;
            }
        }
        // CTC: пропускаем blank(0) и повтор предыдущего индекса.
        if best != 0 && best != prev_idx {
            text.push_str(dict.ch(best));
            scores.push(best_v);
        }
        prev_idx = best;
    }
    let score = if scores.is_empty() {
        0.0
    } else {
        scores.iter().sum::<f32>() / scores.len() as f32
    };
    Ok((text, score))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ключевой фикс качества B: словарь берётся из метадаты ONNX КАК ЕСТЬ (space первым токеном,
    /// пустой токен последним), blank префиксуется в 0. Лишний пробел/замена пустого сдвигали индексы
    /// и портили CTC-декод («КОРОЧЕ» -> «kрhеh»). Проверяем точное отображение классов.
    #[test]
    fn dict_metadata_indexing_exact() {
        // модель cyrillic: 164 токена (space, ..., пустой), + blank = 165 классов (C выхода).
        let raw = " \n!\n#\nА\nБ\nВ\n"; // split -> [" ","!","#","А","Б","В",""] = 7 токенов
        let d = RecDict::from_dict_str(raw);
        // 7 токенов (последний "" хвостовой) + blank = 8 классов
        assert_eq!(d.chars.len(), 8);
        assert_eq!(d.ch(0), ""); // blank
        assert_eq!(d.ch(1), " "); // space ПЕРВЫМ (как в cyrillic-мете)
        assert_eq!(d.ch(2), "!");
        assert_eq!(d.ch(3), "#");
        assert_eq!(d.ch(4), "А");
        assert_eq!(d.ch(5), "Б");
        assert_eq!(d.ch(6), "В");
        assert_eq!(d.ch(7), ""); // хвостовой пустой токен = класс C-1
        // хвостовой пустой токен на месте (не съеден) — иначе сдвиг индексов
        let raw2 = "А\nБ\n"; // "А","Б",""  -> split даёт ["А","Б",""]
        let d2 = RecDict::from_dict_str(raw2);
        assert_eq!(d2.chars.len(), 4); // blank + А + Б + ""
        assert_eq!(d2.ch(3), "");
    }

    #[test]
    fn dict_file_fallback_strips_only_trailing_newline() {
        let tmp = std::env::temp_dir().join("dub_ocr_dict_test.txt");
        std::fs::write(&tmp, " \r\n!\r\nА\r\n").unwrap(); // CRLF, space первым
        let d = RecDict::load(&tmp).unwrap();
        // blank + [space, !, А] = 4 (хвостовой \n съеден, пустого токена нет)
        assert_eq!(d.chars.len(), 4);
        assert_eq!(d.ch(1), " ");
        assert_eq!(d.ch(2), "!");
        assert_eq!(d.ch(3), "А");
        let _ = std::fs::remove_file(&tmp);
    }
}
