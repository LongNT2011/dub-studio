//! dub-ocr — экранный OCR для блюр-боксов вшитого текста. Порт dubengine/text_detect.py (detect_regions:
//! семплинг кадров -> PP-OCR det+rec -> merge строк -> IoU-трекинг -> most-frequent текст на трек) +
//! dubengine/compose.py (looks_like_caption, group_captions, analyze_layout: субтитр-полоса vs титры).
//!
//! Движки det/rec — PP-OCR ONNX через ort (models/ocr/). Рантайм — свой ort-пайплайн (paddle-ocr-rs
//! использует ort rc.10 + download-binaries, что конфликтует с нашим пинном rc.12 load-dynamic и
//! рискует тем же дедлоком, что чужая system32 DLL — потому собственный движок на общей 1.24.2).

mod det;
mod ort_engine;
mod rec;

pub use det::DetBox;

use det::detect;
use image::RgbImage;
use ort_engine::OnnxModel;
use rec::{recognize, RecDict};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Одна per-frame детекция строки: (text, x, y, w, h, t) — сырой поток для frame-accurate блюра.
#[derive(Clone, Debug)]
pub struct RawDet {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub t: f32,
}

/// Регион-трек: (text, x, y, w, h, t0, t1) — стабилизированный трекингом. Порт выхода detect_regions.
#[derive(Clone, Debug)]
pub struct Region {
    pub text: String,
    pub x: i64,
    pub y: i64,
    pub w: i64,
    pub h: i64,
    pub t0: f32,
    pub t1: f32,
}

/// Пути к моделям OCR.
pub struct OcrPaths {
    pub det: PathBuf,
    pub rec: PathBuf,
    pub rec_dict: PathBuf,
}

impl OcrPaths {
    /// Дефолт: <models>/ocr/{det.onnx, rec_cyrillic.onnx, rec_cyrillic.dict.txt}.
    pub fn under(models_root: &Path) -> Self {
        let o = models_root.join("ocr");
        OcrPaths {
            det: o.join("det.onnx"),
            rec: o.join("rec_cyrillic.onnx"),
            rec_dict: o.join("rec_cyrillic.dict.txt"),
        }
    }
    pub fn all_exist(&self) -> bool {
        self.det.is_file() && self.rec.is_file() && self.rec_dict.is_file()
    }
}

// ─── text_detect.py порт ─────────────────────────────────────────────────────

#[cfg(windows)]
const FFMPEG: &str = "ffmpeg.exe";
#[cfg(not(windows))]
const FFMPEG: &str = "ffmpeg";

/// Извлечь кадры видео с частотой fps в PNG (порт _frames).
fn extract_frames(video: &Path, out_dir: &Path, fps: i32) -> Result<Vec<PathBuf>, String> {
    std::fs::create_dir_all(out_dir).map_err(|e| e.to_string())?;
    let vf = format!("fps={fps}");
    let pat = out_dir.join("f_%05d.png");
    let out = Command::new(FFMPEG)
        .arg("-y")
        .arg("-i")
        .arg(video)
        .args(["-vf", &vf])
        .arg(&pat)
        .output()
        .map_err(|e| format!("ffmpeg frames: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "ffmpeg frames код {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).chars().rev().take(400).collect::<String>()
        ));
    }
    let mut frames: Vec<PathBuf> = std::fs::read_dir(out_dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "png").unwrap_or(false))
        .collect();
    frames.sort();
    Ok(frames)
}

/// Нормализованная схожесть текста — трек продолжается пока это ТОТ ЖЕ текст. Порт _sim.
fn sim(a: &str, b: &str) -> f32 {
    let norm = |s: &str| -> String {
        s.chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect()
    };
    let na = norm(a);
    let nb = norm(b);
    if na.is_empty() || nb.is_empty() {
        return 0.0;
    }
    if na.contains(&nb) || nb.contains(&na) {
        return 1.0;
    }
    // упрощённая ratio: доля общих символов (SequenceMatcher-приближение достаточно для трекинга).
    let (long, short) = if na.len() >= nb.len() { (&na, &nb) } else { (&nb, &na) };
    let matches = short.chars().filter(|c| long.contains(*c)).count();
    matches as f32 / long.chars().count().max(1) as f32
}

/// IoU двух боксов (x,y,w,h). Порт _iou.
fn iou(a: (f32, f32, f32, f32), b: (f32, f32, f32, f32)) -> f32 {
    let (ax, ay, aw, ah) = a;
    let (bx, by, bw, bh) = b;
    let x1 = ax.max(bx);
    let y1 = ay.max(by);
    let x2 = (ax + aw).min(bx + bw);
    let y2 = (ay + ah).min(by + bh);
    let inter = (x2 - x1).max(0.0) * (y2 - y1).max(0.0);
    let union = aw * ah + bw * bh - inter;
    if union > 0.0 {
        inter / union
    } else {
        0.0
    }
}

/// Слить боксы слов на одной горизонтальной строке -> одна строка. Порт _merge_rows.
fn merge_rows(items: Vec<(String, f32, f32, f32, f32)>, y_tol: f32) -> Vec<(String, f32, f32, f32, f32)> {
    let mut sorted = items;
    sorted.sort_by(|a, b| (a.2, a.1).partial_cmp(&(b.2, b.1)).unwrap_or(std::cmp::Ordering::Equal));
    let mut out: Vec<(String, f32, f32, f32, f32)> = Vec::new();
    for (txt, x, y, w, h) in sorted {
        let cy = y + h / 2.0;
        let mut merged = false;
        for e in out.iter_mut() {
            let ecy = e.2 + e.4 / 2.0;
            if (ecy - cy).abs() <= y_tol * h.max(e.4) {
                let nx = e.1.min(x);
                let ny = e.2.min(y);
                let nx2 = (e.1 + e.3).max(x + w);
                let ny2 = (e.2 + e.4).max(y + h);
                e.0 = format!("{} {}", e.0, txt);
                e.1 = nx;
                e.2 = ny;
                e.3 = nx2 - nx;
                e.4 = ny2 - ny;
                merged = true;
                break;
            }
        }
        if !merged {
            out.push((txt, x, y, w, h));
        }
    }
    out
}

/// Один трек детекции (внутренний).
struct Track {
    bx: (f32, f32, f32, f32),
    t0: f32,
    t1: f32,
    last_t: f32,
    texts: Vec<String>,
}

/// detect_regions: семплинг кадров -> det+rec -> merge -> IoU-трекинг -> most-frequent текст.
/// Порт text_detect.detect_regions. Возвращает (regions, raw).
#[allow(clippy::too_many_arguments)]
pub fn detect_regions(
    video: &Path,
    work_dir: &Path,
    paths: &OcrPaths,
    fps: i32,
    min_dur: f32,
    iou_thr: f32,
    pad: i64,
    jitter: f32,
    score_thr: f32,
) -> Result<(Vec<Region>, Vec<RawDet>), String> {
    let mut det_model = OnnxModel::load(&paths.det)?;
    let mut rec_model = OnnxModel::load(&paths.rec)?;
    let dict = RecDict::load(&paths.rec_dict)?;

    let fdir = work_dir.join("frames");
    let frames = extract_frames(video, &fdir, fps)?;

    let mut tracks: Vec<Track> = Vec::new();
    let mut raw: Vec<RawDet> = Vec::new();

    for (i, fp) in frames.iter().enumerate() {
        let t = i as f32 / fps as f32;
        let img = match image::open(fp) {
            Ok(im) => im.to_rgb8(),
            Err(_) => continue,
        };
        // det -> боксы; rec каждый бокс.
        let boxes = detect(&mut det_model, &img)?;
        let mut lines_raw: Vec<(String, f32, f32, f32, f32)> = Vec::new();
        for b in &boxes {
            let crop = crop_rgb(&img, b.x, b.y, b.w, b.h);
            let (txt, score) = recognize(&mut rec_model, &dict, &crop)?;
            if txt.trim().is_empty() || score < score_thr {
                continue;
            }
            lines_raw.push((txt.trim().to_string(), b.x, b.y, b.w, b.h));
        }
        // merge_rows + фильтр аспекта (строки широкие: w>=1.2h).
        let lines: Vec<(String, f32, f32, f32, f32)> = merge_rows(lines_raw, 0.6)
            .into_iter()
            .filter(|l| l.3 >= 1.2 * l.4)
            .collect();

        for (txt, x, y, w, h) in lines {
            raw.push(RawDet { text: txt.clone(), x, y, w, h, t });
            let bx = (x, y, w, h);
            // найти трек: то же место (IoU) И ~тот же текст (sim>=0.7) в пределах 2 кадров.
            let mut matched: Option<usize> = None;
            for (ti, tr) in tracks.iter().enumerate() {
                if tr.last_t >= t - 2.0 / fps as f32
                    && iou(tr.bx, bx) >= iou_thr
                    && sim(tr.texts.last().map(|s| s.as_str()).unwrap_or(""), &txt) >= 0.7
                {
                    matched = Some(ti);
                    break;
                }
            }
            if let Some(ti) = matched {
                let tr = &mut tracks[ti];
                tr.t1 = t;
                tr.last_t = t;
                tr.texts.push(txt);
                // де-джиттер: держим прежний бокс, если сдвиг <= jitter.
                let moved = [(tr.bx.0, bx.0), (tr.bx.1, bx.1), (tr.bx.2, bx.2), (tr.bx.3, bx.3)]
                    .iter()
                    .any(|(p, q)| (p - q).abs() > jitter);
                if moved {
                    tr.bx = bx;
                }
            } else {
                tracks.push(Track { bx, t0: t, t1: t, last_t: t, texts: vec![txt] });
            }
        }
    }

    // финализация треков -> Region (most-frequent текст, pad).
    let mut regions = Vec::new();
    for tr in &tracks {
        if tr.t1 - tr.t0 >= min_dur && !tr.texts.is_empty() {
            let (x, y, w, h) = tr.bx;
            let text = most_common(&tr.texts);
            regions.push(Region {
                text,
                x: ((x - pad as f32).max(0.0)) as i64,
                y: ((y - pad as f32).max(0.0)) as i64,
                w: (w + 2.0 * pad as f32) as i64,
                h: (h + 2.0 * pad as f32) as i64,
                t0: (tr.t0 * 100.0).round() / 100.0,
                t1: ((tr.t1 + 1.0 / fps as f32) * 100.0).round() / 100.0,
            });
        }
    }
    Ok((regions, raw))
}

fn crop_rgb(img: &RgbImage, x: f32, y: f32, w: f32, h: f32) -> RgbImage {
    let x0 = x.max(0.0) as u32;
    let y0 = y.max(0.0) as u32;
    let x1 = ((x + w) as u32).min(img.width());
    let y1 = ((y + h) as u32).min(img.height());
    if x1 <= x0 || y1 <= y0 {
        return RgbImage::new(1, 1);
    }
    image::imageops::crop_imm(img, x0, y0, x1 - x0, y1 - y0).to_image()
}

fn most_common(v: &[String]) -> String {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for s in v {
        *counts.entry(s.as_str()).or_insert(0) += 1;
    }
    counts
        .into_iter()
        .max_by_key(|(_, c)| *c)
        .map(|(s, _)| s.to_string())
        .unwrap_or_default()
}

// ─── compose.py порт (layout: субтитр-полоса vs титры) ───────────────────────

/// caption-shape фильтр: реальный оверлей — словоподобный текст (letters/nonspace>=0.6, letters>=3).
/// Порт looks_like_caption.
pub fn looks_like_caption(txt: &str) -> bool {
    let t = txt.trim();
    let letters = t.chars().filter(|c| c.is_alphabetic()).count();
    let nonspace = t.chars().filter(|c| !c.is_whitespace()).count();
    if letters < 3 || nonspace == 0 {
        return false;
    }
    letters as f32 / nonspace as f32 >= 0.6
}

/// Бокс субтитр-полосы под блюр: (x,y,w,h,t0,t1).
pub type CaptionBox = (i64, i64, i64, i64, f32, f32);

/// analyze_layout -> (localize, caption_boxes, sub_y). Порт compose.analyze_layout: бакетим детекции по
/// горизонтальным полосам; в нижней части кадра выбираем полосу(ы), где текст ЧАСТО меняется (много
/// distinct строк) И слова СКАЗАНЫ (spoken) — это субтитр-полоса -> caption_boxes (блюр). Остальное ->
/// localize (титры). Возвращаем sub_y = центр самой «богатой» полосы.
pub fn analyze_layout(
    ocr: &[Region],
    frame_h: i64,
    raw: &[RawDet],
    spoken: &std::collections::HashSet<String>,
) -> (Vec<Region>, Vec<CaptionBox>, Option<i64>) {
    let band_frac = 0.10f32;
    let lower_from = 0.45f32;

    // band_src: предпочитаем raw (single-word karaoke дробит трек), фильтр looks_like_caption.
    let band_src: Vec<(&str, f32, f32, f32, f32, f32, f32)> = if !raw.is_empty() {
        raw.iter()
            .filter(|r| looks_like_caption(&r.text))
            .map(|r| (r.text.as_str(), r.x, r.y, r.w, r.h, r.t, r.t))
            .collect()
    } else {
        ocr.iter()
            .filter(|r| looks_like_caption(&r.text))
            .map(|r| (r.text.as_str(), r.x as f32, r.y as f32, r.w as f32, r.h as f32, r.t0, r.t1))
            .collect()
    };
    if band_src.is_empty() {
        return (ocr.to_vec(), Vec::new(), None);
    }

    let band_h = (frame_h as f32 * band_frac).max(1.0);
    // bucket -> детекции.
    let mut bands: HashMap<i64, Vec<(&str, f32, f32, f32, f32, f32, f32)>> = HashMap::new();
    for r in &band_src {
        let cy = r.2 + r.4 / 2.0;
        let b = (cy / band_h) as i64;
        bands.entry(b).or_default().push(*r);
    }

    let band_cy = |b: i64| -> f32 {
        let mut cys: Vec<f32> = bands[&b].iter().map(|r| r.2 + r.4 / 2.0).collect();
        cys.sort_by(|a, b| a.partial_cmp(b).unwrap());
        cys[cys.len() / 2]
    };
    let distinct_texts = |rs: &[(&str, f32, f32, f32, f32, f32, f32)]| -> usize {
        let set: std::collections::HashSet<String> =
            rs.iter().filter(|r| !r.0.trim().is_empty()).map(|r| r.0.trim().to_lowercase()).collect();
        set.len()
    };
    let spoken_frac = |rs: &[(&str, f32, f32, f32, f32, f32, f32)]| -> f32 {
        if spoken.is_empty() {
            return 1.0;
        }
        let distinct: std::collections::HashSet<String> =
            rs.iter().filter(|r| !r.0.trim().is_empty()).map(|r| r.0.trim().to_lowercase()).collect();
        if distinct.is_empty() {
            return 0.0;
        }
        let mut hit = 0;
        for t in &distinct {
            let ws: Vec<&str> = t.split(|c: char| !c.is_alphabetic()).filter(|w| !w.is_empty()).collect();
            if !ws.is_empty() && ws.iter().filter(|w| spoken.contains(&w.to_lowercase())).count() as f32 >= 0.5 * ws.len() as f32 {
                hit += 1;
            }
        }
        hit as f32 / distinct.len() as f32
    };
    // OCR-достоверность: если НИ в одной нижней полосе spoken-match не срабатывает (наш rec шумит на
    // мелких/motion-blur субтитрах, в отличие от чистого PP-OCR питона), spoken-гейт становится ложно
    // строгим и убивает реальную субтитр-полосу. Тогда деградируем к ЧИСТО ГЕОМЕТРИЧЕСКОМУ сигналу
    // (повторяющийся текст в нижней полосе) — принципиальный фолбэк, не подгонка под клип.
    let ocr_text_reliable = !spoken.is_empty()
        && bands
            .iter()
            .filter(|(&b, _)| band_cy(b) >= lower_from * frame_h as f32)
            .any(|(_, rs)| distinct_texts(rs) >= 3 && spoken_frac(rs) >= 0.5);
    let is_sub_line = |b: i64| -> bool {
        let rs = &bands[&b];
        let nt = distinct_texts(rs);
        let geom = nt >= 3 && nt as f32 >= 0.3 * rs.len() as f32;
        if ocr_text_reliable {
            geom && spoken_frac(rs) >= 0.5
        } else {
            geom // rec ненадёжен -> геометрия: меняющийся текст в нижней полосе = субтитр-полоса
        }
    };

    let lines: Vec<i64> = bands
        .keys()
        .copied()
        .filter(|&b| band_cy(b) >= lower_from * frame_h as f32 && is_sub_line(b))
        .collect();
    if lines.is_empty() {
        return (ocr.to_vec(), Vec::new(), None);
    }
    let centers: Vec<f32> = lines.iter().map(|&b| band_cy(b)).collect();
    let richest = *lines
        .iter()
        .max_by_key(|&&b| distinct_texts(&bands[&b]))
        .unwrap();
    let sub_y = band_cy(richest) as i64;

    let on_any_line = |cy: f32| -> bool { centers.iter().any(|c| (cy - c).abs() <= 0.7 * band_h) };

    // caption_boxes: каждая lettered строка на субтитр-линии (raw если есть, иначе ocr).
    let caption_boxes: Vec<CaptionBox> = if !raw.is_empty() {
        raw.iter()
            .filter(|r| on_any_line(r.y + r.h / 2.0) && r.text.chars().any(|c| c.is_alphabetic()))
            .map(|r| (r.x as i64, r.y as i64, r.w as i64, r.h as i64, r.t, r.t))
            .collect()
    } else {
        ocr.iter()
            .filter(|r| on_any_line(r.y as f32 + r.h as f32 / 2.0) && r.text.chars().any(|c| c.is_alphabetic()))
            .map(|r| (r.x, r.y, r.w, r.h, r.t0, r.t1))
            .collect()
    };
    let localize: Vec<Region> = ocr
        .iter()
        .filter(|r| !on_any_line(r.y as f32 + r.h as f32 / 2.0))
        .cloned()
        .collect();
    (localize, caption_boxes, Some(sub_y))
}
