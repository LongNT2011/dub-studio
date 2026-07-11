//! Caption-композит (порт pipeline.run:388-643) — «склейка», собирающая финальные ТИТРЫ с bbox и
//! полный набор блюр-боксов из (а) vision-словаря raw_ctx (titles/captions/brands/sub_style) и
//! (б) OCR-выхода (localize-регионы + caption_boxes субтитр-полосы). Выполняется на analyze ПОСЛЕ
//! translate-стадии (raw_ctx готов) и OCR-детекции (localize/caption_boxes готовы) — точное место
//! композита в питоне.
//!
//! Что делает (дословно по pipeline.py):
//!  1. ctitles: merge по y (<=0.06) -> для каждого титра bbox матчингом y_frac*vh к localize-боксам
//!     (общий-word приоритет; fallback центр-полоса) -> Title.bbox/lh/start/end/color/bg/… (стр.497-543).
//!  2. blur_boxes: нарисованные title-блоки + таглайны (<=3.0/3.5с, центр, не в титре -> перевод+рисуем)
//!     + group_blur центр-OCR-титров + leftover под блоками + cap_blur вторичных капшенов + band_blur
//!     (стр.443-452, 579-589).
//!  3. sub_px: медиана высот OCR-боксов на sub_y (±7%vh) / cap_px / медиана caption_boxes (стр.608-610).
//!  4. sub_style mirror-from-captions + cap_px (стр.466-484).
//!  5. white-card fallback: scene_color=#FFFFFF/scene_flat при solid светлом титре (стр.490-493).
//!
//! Fail-safe: любой сбой оставляет то, что уже собрано (band_blur из OCR-стадии), analyze не падает.

use dub_core::{BlurBox, Project, SubStyle, Title};
use dub_ocr::{blur, group_captions, CaptionBox, CaptionGroup, Region};
use serde_json::Value;

use crate::analyze::{AnalyzePaths, Progress};

fn emit(progress: &Progress, stage: &str, msg: &str) {
    progress(serde_json::json!({ "stage": stage, "msg": msg }));
}

/// r-tuple как в питоне: (text, x, y, w, h, t0, t1). Region.x/y/w/h — i64; приводим к f32 для геометрии.
struct R {
    text: String,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    t0: f32,
    t1: f32,
}

impl R {
    fn from_region(r: &Region) -> Self {
        R {
            text: r.text.clone(),
            x: r.x as f32,
            y: r.y as f32,
            w: r.w as f32,
            h: r.h as f32,
            t0: r.t0,
            t1: r.t1,
        }
    }
    fn cy(&self) -> f32 {
        self.y + self.h / 2.0
    }
}

/// Промежуточный нарисованный блок (loc_block): bbox + текст + время + стиль. Порт loc_blocks-словаря.
struct LocBlock {
    bbox: (i32, i32, i32, i32), // x, y, w, h
    text: String,
    start: f32,
    end: f32,
    lh: i32,
    color: Option<String>,
    bg: Option<String>,
    solid: bool,
    align: String,
    bold: bool,
    italic: bool,
    font: Option<String>,
}

/// Токенизировать строку в слова (>=3 симв., нижний регистр) — как re.findall(r"\w+") + len>=3 в питоне.
fn words3(s: &str) -> std::collections::HashSet<String> {
    s.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| w.chars().count() >= 3)
        .map(|w| w.to_lowercase())
        .collect()
}

/// alpha-count (sum(c.isalpha())).
fn alpha_count(s: &str) -> usize {
    s.chars().filter(|c| c.is_alphabetic()).count()
}

/// Аргументы композита.
pub struct ComposeCtx<'a> {
    pub vw: i64,
    pub vh: i64,
    pub total: f64,
    /// do_translate = dub || subs==translate (для таглайн-перевода).
    pub do_translate: bool,
    /// fresh_subs — режим свежих сабов (нечего блюрить). В нашем Project по умолчанию false.
    pub fresh_subs: bool,
    /// src_lang для таглайн-MT.
    pub src_lang: &'a str,
    /// пути (llama-бинарь/веса) для опционального таглайн-MT.
    pub paths: &'a AnalyzePaths,
}

/// Запустить композит. Мутирует proj.captions: titles (с bbox), blur_boxes (полный набор), sub_style
/// (mirror/white-card), raw_plan["sub_px"]. localize/caption_boxes — из OCR-стадии; band_blur — уже
/// посчитанный субтитр-блюр (передаём готовым, чтобы не дублировать коалесценцию).
pub fn run(
    proj: &mut Project,
    localize: &[Region],
    caption_boxes: &[CaptionBox],
    band_blur: &[BlurBox],
    ctx: &ComposeCtx,
    progress: &Progress,
) {
    let vw = ctx.vw as f32;
    let vh = ctx.vh as f32;
    let total = ctx.total as f32;
    let cx_lo = 0.40 * vw;
    let cx_hi = 0.60 * vw;
    let centered = |x: f32, w: f32| -> bool { x < cx_hi && x + w > cx_lo };

    // localize -> R-вью (r[0..6] как в питоне). Порядок как отдал analyze_layout (отсортирован по t0,y).
    let loc: Vec<R> = localize.iter().map(R::from_region).collect();

    // ── ctx-словарь (raw_ctx = ce) ────────────────────────────────────────────
    let ce = &proj.raw_ctx;
    let titles_v: Vec<Value> = ce
        .get("titles")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let captions_v: Vec<Value> = ce
        .get("captions")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let brands_v: Vec<Value> = ce
        .get("brands")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    // ── group_blur: центр-OCR-титры (как в питоне groups) ──────────────────────
    // TITLE-кандидаты из localize: (<=1.5с в начале ИЛИ >= total-6 в конце) И lh>=4%vh И центр.
    let title_src: Vec<(String, f32, f32, f32, f32, f32, f32)> = loc
        .iter()
        .filter(|r| {
            (r.t0 <= 1.5 || r.t1 >= total - 6.0) && r.h >= 0.04 * vh && centered(r.x, r.w)
        })
        .map(|r| (r.text.clone(), r.x, r.y, r.w, r.h, r.t0, r.t1))
        .collect();
    let mut groups: Vec<CaptionGroup> = group_captions(&title_src, 0.0, 0.0)
        .into_iter()
        .filter(|g| alpha_count(&g.text) >= 4)
        .collect();
    // отсечь кандидатов, идущих КОНКУРЕНТНО с субтитр-полосой (_runs_with_subs).
    let band_t: Vec<f32> = {
        let mut v: Vec<f32> = caption_boxes.iter().map(|b| b.4).collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        v
    };
    let runs_with_subs = |gs: f32, ge: f32| -> bool {
        let inside: Vec<f32> = band_t.iter().copied().filter(|&t| gs <= t && t <= ge).collect();
        inside.len() >= 3
            && (inside.iter().cloned().fold(f32::MIN, f32::max)
                - inside.iter().cloned().fold(f32::MAX, f32::min))
                >= 0.4 * (ge - gs).max(0.1)
    };
    groups.retain(|g| !runs_with_subs(g.start, g.end));

    // ── sub_style: mirror-from-captions + white-card fallback + cap_px ──────────
    // sub_style уже мог прийти из vision (apply_extra). Если пуст И есть captions -> зеркалим.
    let mut cap_px: Option<i64> = None;
    ensure_sub_style_mirror(proj, &captions_v, &loc, vw, vh, &mut cap_px);
    white_card_fallback(proj, &titles_v);

    // ── ctitles -> loc_blocks с bbox (порт 497-543) ────────────────────────────
    let mut loc_blocks: Vec<LocBlock> = Vec::new();
    // ctitles = titles c непустым tgt.
    let mut ctitles: Vec<Value> = titles_v
        .iter()
        .filter(|t| {
            t.get("tgt").and_then(|x| x.as_str()).map(|s| !s.trim().is_empty()).unwrap_or(false)
        })
        .cloned()
        .collect();
    // merge по y_frac (<=0.06) в ОДИН (склеенные слова титра не встают стопкой на один Y).
    ctitles.sort_by(|a, b| {
        yfrac(a).partial_cmp(&yfrac(b)).unwrap_or(std::cmp::Ordering::Equal)
    });
    let ctitles = merge_titles_by_y(ctitles);

    // cap_blur + cap_ys (вторичные капшены -> блюр их региона).
    let mut cap_blur: Vec<BlurBox> = Vec::new();
    let mut cap_ys: Vec<f32> = Vec::new();

    if !ctitles.is_empty() {
        let ycs: Vec<f32> = ctitles.iter().map(|t| yfrac(t) as f32 * vh).collect();
        // токенизируем каждую localize-строку ОДИН раз.
        let rowwords: Vec<std::collections::HashSet<String>> =
            loc.iter().map(|r| words3(&r.text)).collect();
        for (ti, t) in ctitles.iter().enumerate() {
            let yc = ycs[ti];
            let tw = words3(t.get("text").and_then(|x| x.as_str()).unwrap_or(""));
            // кандидаты: боксы, чей БЛИЖАЙШИЙ титр по y — этот, в пределах 13%vh.
            let cand: Vec<usize> = loc
                .iter()
                .enumerate()
                .filter(|(_, r)| {
                    let cyr = r.cy();
                    (cyr - yc).abs() <= vh * 0.13
                        && nearest_title(&ycs, cyr) == ti
                })
                .map(|(ri, _)| ri)
                .collect();
            let shared: Vec<usize> =
                cand.iter().copied().filter(|&ri| !tw.is_disjoint(&rowwords[ri])).collect();
            let near_idx: &[usize] = if !shared.is_empty() { &shared } else { &cand };
            let (x0, y0, x1, y1, st, en, lh);
            if !near_idx.is_empty() {
                let near: Vec<&R> = near_idx.iter().map(|&ri| &loc[ri]).collect();
                x0 = near.iter().map(|r| r.x).fold(f32::MAX, f32::min);
                y0 = near.iter().map(|r| r.y).fold(f32::MAX, f32::min);
                x1 = near.iter().map(|r| r.x + r.w).fold(f32::MIN, f32::max);
                y1 = near.iter().map(|r| r.y + r.h).fold(f32::MIN, f32::max);
                st = near.iter().map(|r| r.t0).fold(f32::MAX, f32::min);
                en = near.iter().map(|r| r.t1).fold(f32::MIN, f32::max);
                lh = near.iter().map(|r| r.h).fold(f32::MAX, f32::min);
            } else {
                // нет OCR-бокса на этом y -> центр-полоса.
                x0 = (vw * 0.08) as i32 as f32;
                x1 = (vw * 0.92) as i32 as f32;
                y0 = (yc - vh * 0.045) as i32 as f32;
                y1 = (yc + vh * 0.045) as i32 as f32;
                st = 0.0;
                en = total.min(4.0);
                lh = (vh * 0.05) as i32 as f32;
            }
            loc_blocks.push(LocBlock {
                bbox: (x0 as i32, y0 as i32, (x1 - x0) as i32, (y1 - y0) as i32),
                text: t.get("tgt").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                start: st,
                end: en,
                lh: lh as i32,
                color: sval(t, "color"),
                bg: sval(t, "bg"),
                solid: bval(t, "solid", false),
                align: sval(t, "align").unwrap_or_else(|| "center".into()),
                bold: bval(t, "bold", true),
                italic: bval(t, "italic", false),
                font: sval(t, "font"),
            });
        }

        // cap_blur вторичных капшенов + cap_ys (порт 549-554).
        for c in &captions_v {
            let yc2 = yfrac(c) as f32 * vh;
            for r in loc.iter().filter(|r| (r.cy() - yc2).abs() <= vh * 0.06) {
                cap_blur.push(region_box(r));
                cap_ys.push(r.cy());
            }
        }

        // таглайны/leftover-под-блоками, оставшийся центр-текст титр-карты (tcard_rows) -> перевести и
        // нарисовать с заливкой (порт 555-578). MT опционален и fail-safe.
        let bys: Vec<f32> = brands_v.iter().map(|b| yfrac(b) as f32 * vh).collect();
        let mut loc_ys: Vec<f32> =
            loc_blocks.iter().map(|b| b.bbox.1 as f32 + b.bbox.3 as f32 / 2.0).collect();
        loc_ys.extend(cap_ys.iter().copied());
        let tcard_rows: Vec<&R> = loc
            .iter()
            .filter(|r| {
                r.t0 <= 3.5
                    && alpha_count(&r.text) >= 3
                    && centered(r.x, r.w)
                    && !bys.iter().any(|&by| (r.cy() - by).abs() <= vh * 0.05)
                    && !loc_ys.iter().any(|&ly| (r.cy() - ly).abs() <= vh * 0.06)
            })
            .collect();
        if !tcard_rows.is_empty() && ctx.do_translate {
            translate_taglines(proj, &tcard_rows, ctx, &mut loc_blocks, progress);
        }
    }

    // ── финальный blur_boxes ───────────────────────────────────────────────────
    let group_blur: Vec<BlurBox> = groups
        .iter()
        .map(|g| bbox_box(g.bbox, g.start, g.end))
        .collect();

    let final_blur: Vec<BlurBox> = if !ctitles.is_empty() {
        // ctx-ветка: loc_blocks + band_blur + leftover + cap_blur + group_blur (порт 586-587).
        let drawn: Vec<(f32, f32, f32, f32)> = loc_blocks
            .iter()
            .map(|b| (b.bbox.1 as f32, (b.bbox.1 + b.bbox.3) as f32, b.start, b.end))
            .collect();
        let leftover: Vec<BlurBox> = loc
            .iter()
            .filter(|r| {
                centered(r.x, r.w)
                    && alpha_count(&r.text) >= 3
                    && drawn.iter().any(|&(yy0, yy1, sst, een)| {
                        let cyr = r.cy();
                        yy0 <= cyr && cyr <= yy1 && r.t0 < een + 0.3 && r.t1 > sst - 0.3
                    })
            })
            .map(region_box)
            .collect();
        let mut v: Vec<BlurBox> = loc_blocks.iter().map(locblock_box).collect();
        v.extend_from_slice(band_blur);
        v.extend(leftover);
        v.extend(cap_blur);
        v.extend(group_blur);
        v
    } else {
        // не-ctx (нет ctitles): loc_blocks(пусто) + таглайны + group_blur + band_blur (порт 450-452).
        // Таглайны здесь = центр-localize <=3.0с не в титре — блюрим оригинал, чтоб не ликовал.
        let in_title = |r: &R| -> bool {
            groups.iter().any(|g| {
                let (_, gy, _, gh) = g.bbox;
                gy <= r.cy() && r.cy() <= gy + gh
            })
        };
        let taglines: Vec<BlurBox> = if ctx.do_translate && !ctx.fresh_subs {
            loc.iter()
                .filter(|r| r.t0 <= 3.0 && alpha_count(&r.text) >= 4 && centered(r.x, r.w) && !in_title(r))
                .map(region_box)
                .collect()
        } else {
            Vec::new()
        };
        let mut v: Vec<BlurBox> = loc_blocks.iter().map(locblock_box).collect();
        v.extend(taglines);
        v.extend(group_blur);
        v.extend_from_slice(band_blur);
        v
    };

    // ── записать в Project ─────────────────────────────────────────────────────
    proj.captions.titles = loc_blocks.iter().map(locblock_to_title).collect();
    proj.captions.blur_boxes = final_blur;

    // ── sub_px (порт 608-610) ──────────────────────────────────────────────────
    let sub_y = proj.captions.sub_y;
    let mut sh: Vec<f32> = if let Some(sy) = sub_y {
        loc.iter()
            .filter(|r| (r.cy() - sy as f32).abs() <= vh * 0.07)
            .map(|r| r.h)
            .collect()
    } else {
        Vec::new()
    };
    sh.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut bh: Vec<i64> = caption_boxes.iter().map(|b| b.3).collect();
    bh.sort_unstable();
    let sub_px: Option<i64> = if !sh.is_empty() {
        Some(sh[sh.len() / 2] as i64)
    } else {
        cap_px
    }
    .or_else(|| if bh.is_empty() { None } else { Some(bh[bh.len() / 2]) });
    if let Some(px) = sub_px {
        proj.captions
            .raw_plan
            .insert("sub_px".into(), Value::from(px));
    }

    emit(
        progress,
        "ocr_detect",
        &format!(
            "композит: титров={} (bbox), блюр {} боксов, sub_px={:?}",
            proj.captions.titles.len(),
            proj.captions.blur_boxes.len(),
            sub_px
        ),
    );
}

// ─── helpers ────────────────────────────────────────────────────────────────

fn yfrac(t: &Value) -> f64 {
    t.get("y_frac").and_then(|v| v.as_f64()).unwrap_or(0.0)
}
fn sval(t: &Value, k: &str) -> Option<String> {
    t.get(k).and_then(|v| v.as_str()).map(|s| s.to_string())
}
fn bval(t: &Value, k: &str, d: bool) -> bool {
    t.get(k).and_then(|v| v.as_bool()).unwrap_or(d)
}

/// Ближайший титр по y (argmin |ycs[j] - cy|). Порт min(range(len), key=|abs|).
fn nearest_title(ycs: &[f32], cy: f32) -> usize {
    ycs.iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            (**a - cy).abs().partial_cmp(&(**b - cy).abs()).unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(i, _)| i)
        .unwrap_or(0)
}

/// Слить титры на (почти) одной линии (y_frac diff <= 0.06) — tgt через \n. Порт _merged.
fn merge_titles_by_y(sorted: Vec<Value>) -> Vec<Value> {
    let mut merged: Vec<Value> = Vec::new();
    for t in sorted {
        if let Some(last) = merged.last_mut() {
            if (yfrac(&t) - yfrac(last)).abs() <= 0.06 {
                let prev = last.get("tgt").and_then(|x| x.as_str()).unwrap_or("").to_string();
                let cur = t.get("tgt").and_then(|x| x.as_str()).unwrap_or("");
                let joined = format!("{prev}\n{cur}").trim().to_string();
                if let Value::Object(m) = last {
                    m.insert("tgt".into(), Value::from(joined));
                }
                continue;
            }
        }
        merged.push(t);
    }
    merged
}

/// region -> BlurBox (x,y,w,h,t0,t1). Порт region_blur_box (без роста — регион уже с pad detect_regions).
fn region_box(r: &R) -> BlurBox {
    let b = blur::region_blur_box(r.x, r.y, r.w, r.h, r.t0, r.t1);
    BlurBox { x: b.0 as i64, y: b.1 as i64, w: b.2 as i64, h: b.3 as i64, t0: b.4 as f64, t1: b.5 as f64, hidden: false, extra: Default::default() }
}

/// (x,y,w,h)+time -> BlurBox с округлением времени (как group_blur/loc_block: bbox + start/end).
fn bbox_box(bbox: (f32, f32, f32, f32), t0: f32, t1: f32) -> BlurBox {
    BlurBox {
        x: bbox.0 as i64,
        y: bbox.1 as i64,
        w: bbox.2 as i64,
        h: bbox.3 as i64,
        t0: ((t0 * 100.0).round() / 100.0) as f64,
        t1: ((t1 * 100.0).round() / 100.0) as f64,
        hidden: false,
        extra: Default::default(),
    }
}

fn locblock_box(b: &LocBlock) -> BlurBox {
    BlurBox {
        x: b.bbox.0 as i64,
        y: b.bbox.1 as i64,
        w: b.bbox.2 as i64,
        h: b.bbox.3 as i64,
        t0: ((b.start * 100.0).round() / 100.0) as f64,
        t1: ((b.end * 100.0).round() / 100.0) as f64,
        hidden: false,
        extra: Default::default(),
    }
}

/// loc_block -> типизированный Title (bbox/tgt/цвет/стиль). Рисуется emit_title (bbox есть -> не скипнет).
fn locblock_to_title(b: &LocBlock) -> Title {
    Title {
        text: b.text.clone(),
        tgt: b.text.clone(), // уже переведён; map_title в render берёт tgt при непустом
        bbox: Some(vec![b.bbox.0 as i64, b.bbox.1 as i64, b.bbox.2 as i64, b.bbox.3 as i64]),
        color: b.color.clone(),
        bg: b.bg.clone(),
        font: b.font.clone(),
        italic: b.italic,
        align: b.align.clone(),
        start: b.start as f64,
        end: b.end as f64,
        lh: Some(b.lh as i64),
        solid: b.solid,
        bold: b.bold,
        size_px: None,
        outline: None,
        outline_w: None,
        uppercase: false,
        extra: Default::default(),
    }
}

/// sub_style mirror-from-captions (порт 466-484): при пустом sub_style и непустых captions собрать
/// стиль из мод-значений капшенов + cap_px = медиана высот центр-нижних OCR-боксов.
fn ensure_sub_style_mirror(
    proj: &mut Project,
    captions_v: &[Value],
    loc: &[R],
    vw: f32,
    vh: f32,
    cap_px: &mut Option<i64>,
) {
    if proj.captions.sub_style.is_some() || captions_v.is_empty() {
        return;
    }
    let cx_lo = 0.40 * vw;
    let cx_hi = 0.60 * vw;
    let centered = |x: f32, w: f32| -> bool { x < cx_hi && x + w > cx_lo };
    let mode_str = |vals: Vec<String>, d: &str| -> String {
        let vals: Vec<String> = vals.into_iter().filter(|v| !v.is_empty()).collect();
        if vals.is_empty() {
            return d.to_string();
        }
        let mut counts: std::collections::HashMap<&String, usize> = std::collections::HashMap::new();
        for v in &vals {
            *counts.entry(v).or_insert(0) += 1;
        }
        counts.into_iter().max_by_key(|(_, c)| *c).map(|(v, _)| v.clone()).unwrap()
    };
    // cap_px: медиана высот центр-нижних OCR-боксов (центр X, cy > 0.40*vh). Порт _caph.
    let mut caph: Vec<f32> = loc
        .iter()
        .filter(|r| centered(r.x, r.w) && r.cy() > vh * 0.40)
        .map(|r| r.h)
        .collect();
    caph.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if !caph.is_empty() {
        *cap_px = Some(caph[caph.len() / 2] as i64);
    }
    let colors: Vec<String> = captions_v.iter().filter_map(|c| sval(c, "color")).collect();
    let outlines: Vec<String> = captions_v
        .iter()
        .filter_map(|c| sval(c, "outline"))
        .filter(|o| o != "none")
        .collect();
    let fonts: Vec<String> = captions_v.iter().filter_map(|c| sval(c, "font")).collect();
    let n = captions_v.len() as f64;
    let bold = captions_v.iter().filter(|c| bval(c, "bold", false)).count() as f64 >= n / 2.0;
    let italic = captions_v.iter().filter(|c| bval(c, "italic", false)).count() as f64 > n / 2.0;
    let mut style = SubStyle {
        color: mode_str(colors, "#FFFFFF"),
        outline: mode_str(outlines, "#000000"),
        bold,
        italic,
        align: "center".to_string(),
        font: if fonts.is_empty() { None } else { Some(mode_str(fonts, "")) },
        ..SubStyle::default()
    };
    style.extra.insert("background".into(), Value::from("none"));
    style.extra.insert("solid".into(), Value::from(false));
    proj.captions.sub_style = Some(style);
}

/// white-card fallback (порт 490-493): sub_style без scene_color + среди titles есть solid светлый
/// (lum>0.7) фон -> scene_color=#FFFFFF, scene_flat=true (активирует cover_c-крышку в build).
fn white_card_fallback(proj: &mut Project, titles_v: &[Value]) {
    let Some(style) = proj.captions.sub_style.as_mut() else { return };
    if style.scene_color.is_some() {
        return;
    }
    let has_white_card = titles_v.iter().any(|t| {
        bval(t, "solid", false)
            && t.get("bg")
                .and_then(|v| v.as_str())
                .map(|bg| dub_captions_lum(bg) > 0.7)
                .unwrap_or(false)
    });
    if has_white_card {
        style.scene_color = Some("#FFFFFF".to_string());
        style.scene_flat = true;
    }
}

/// Люминанс hex (порт captions._lum). Дублирует look::lum (тот не экспортируется из dub-captions в
/// нужной форме для #hex -> делаем локально по той же формуле, чтоб не тянуть зависимость на приватный).
fn dub_captions_lum(hexstr: &str) -> f32 {
    dub_captions::lum(hexstr)
}

/// Опциональный таглайн-MT (порт 564-578): поднять text-only Gemma, перевести tcard_rows, нарисовать
/// с заливкой из sub_style. Fail-safe: без бинаря/весов/сбой — блюр всё равно накроет оригинал.
fn translate_taglines(
    proj: &mut Project,
    tcard_rows: &[&R],
    ctx: &ComposeCtx,
    loc_blocks: &mut Vec<LocBlock>,
    progress: &Progress,
) {
    use dub_llm::{ChatClient, LlamaServer, ServerOpts};
    use dub_translate::{flat_run, Seg};

    let paths = ctx.paths;
    if !paths.llama_bin.is_file() || !paths.mt_model.is_file() {
        emit(progress, "ocr_detect", "таглайны: MT недоступен -> только блюр (без перевода)");
        return;
    }
    emit(progress, "ocr_detect", "таглайны: text-only Gemma для перевода надписей титр-карты");
    let opts = ServerOpts::new(&paths.llama_bin, &paths.mt_model); // без mmproj (плоский MT)
    let srv = match LlamaServer::start(&opts) {
        Ok(s) => s,
        Err(e) => {
            emit(progress, "ocr_detect", &format!("таглайны: llama не поднялся ({e}); только блюр"));
            return;
        }
    };
    let client = match ChatClient::new(srv.base_url()) {
        Ok(c) => c,
        Err(_) => return,
    };
    let mut segs: Vec<Seg> = tcard_rows.iter().map(|r| Seg::new(r.text.clone(), 0)).collect();
    let ok = flat_run(&client, &mut segs, ctx.src_lang, &proj.tgt_lang, false).is_ok();
    drop(srv);
    if !ok {
        emit(progress, "ocr_detect", "таглайны: перевод не удался; только блюр");
        return;
    }
    // стиль заливки = sub_style (как ss в питоне).
    let ss = proj.captions.sub_style.clone().unwrap_or_default();
    let ss_solid = ss.extra.get("solid").and_then(|v| v.as_bool()).unwrap_or(false);
    let ss_bg = ss
        .extra
        .get("background")
        .and_then(|v| v.as_str())
        .filter(|b| ss_solid && *b != "none")
        .map(|s| s.to_string());
    for (r, s2) in tcard_rows.iter().zip(segs.iter()) {
        let t2 = s2.tgt.trim();
        if t2.is_empty() {
            continue;
        }
        loc_blocks.push(LocBlock {
            bbox: (r.x as i32, r.y as i32, r.w as i32, r.h as i32),
            text: t2.to_string(),
            start: r.t0,
            end: r.t1,
            lh: (r.h * 0.8) as i32,
            color: Some(ss.color.clone()),
            bg: ss_bg.clone(),
            solid: ss_solid,
            align: ss.align.clone(),
            bold: ss.bold,
            italic: ss.italic,
            font: ss.font.clone(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn reg(text: &str, x: i64, y: i64, w: i64, h: i64, t0: f32, t1: f32) -> Region {
        Region { text: text.into(), x, y, w, h, t0, t1 }
    }

    fn dummy_paths() -> AnalyzePaths {
        // Все пути несуществующие -> таглайн-MT никогда не поднимется (fail-safe no-op в тестах).
        let p = PathBuf::from("/nonexistent");
        AnalyzePaths {
            input: p.clone(),
            work_dir: p.clone(),
            tdt_dir: p.clone(),
            sortformer_onnx: p.clone(),
            llama_bin: p.clone(),
            mt_model: p.clone(),
            mmproj: p.clone(),
            models_root: p.clone(),
            caption_fps: 4,
        }
    }

    // Эталон питона (см. отчёт, standalone-прогон блока pipeline.py:500-543):
    //  ctitle 'КОРОЧЕ ПОДКЛЮЧЕН'/tgt 'IN SHORT CONNECTED', y_frac=84/1280, два localize-бокса разделяют
    //  слова -> bbox (150,80,370,46), start 0.0, end 1.5, lh 42. Боковой low-бокс вне 13%vh -> не матчится.
    #[test]
    fn ctitle_bbox_matches_localize_boxes() {
        let vw = 720i64;
        let vh = 1280i64;
        let localize = vec![
            reg("КОРОЧЕ", 150, 80, 180, 44, 0.0, 1.4),
            reg("ПОДКЛЮЧЕН", 300, 84, 220, 42, 0.2, 1.5),
            reg("random", 600, 700, 90, 30, 5.0, 6.0),
        ];
        let mut proj = Project::default();
        proj.mode = "dub".into();
        proj.tgt_lang = "en".into();
        proj.raw_ctx.insert(
            "titles".into(),
            serde_json::json!([{
                "text": "КОРОЧЕ ПОДКЛЮЧЕН", "tgt": "IN SHORT CONNECTED",
                "y_frac": 84.0 / 1280.0, "color": "#FFFFFF", "bg": null, "solid": false,
                "align": "center", "bold": true, "italic": false, "font": null
            }]),
        );
        let paths = dummy_paths();
        let ctx = ComposeCtx {
            vw, vh, total: 30.0, do_translate: true, fresh_subs: false,
            src_lang: "ru", paths: &paths,
        };
        let noop: Box<Progress> = Box::new(|_| {});
        run(&mut proj, &localize, &[], &[], &ctx, &noop);

        assert_eq!(proj.captions.titles.len(), 1);
        let t = &proj.captions.titles[0];
        assert_eq!(t.bbox.as_deref(), Some(&[150i64, 80, 370, 46][..]));
        assert_eq!(t.tgt, "IN SHORT CONNECTED");
        assert_eq!(t.start, 0.0);
        assert_eq!(t.end, 1.5);
        assert_eq!(t.lh, Some(42));
        // титр -> нарисован -> его bbox в блюр-боксах (не просвечивает EN-оригинал).
        assert!(proj
            .captions
            .blur_boxes
            .iter()
            .any(|b| b.x == 150 && b.y == 80 && b.w == 370 && b.h == 46));
    }

    // Fallback центр-полоса: нет localize-боксов на y титра -> bbox = центр (8%..92% ширины), y=yc±4.5%vh.
    #[test]
    fn ctitle_fallback_center_band() {
        let vw = 720i64;
        let vh = 1280i64;
        let mut proj = Project::default();
        proj.mode = "dub".into();
        proj.raw_ctx.insert(
            "titles".into(),
            serde_json::json!([{
                "text": "SOME TITLE", "tgt": "НЕКИЙ ТИТР", "y_frac": 0.2, "align": "center"
            }]),
        );
        let paths = dummy_paths();
        let ctx = ComposeCtx {
            vw, vh, total: 30.0, do_translate: true, fresh_subs: false,
            src_lang: "en", paths: &paths,
        };
        let noop: Box<Progress> = Box::new(|_| {});
        run(&mut proj, &[], &[], &[], &ctx, &noop);
        assert_eq!(proj.captions.titles.len(), 1);
        let t = &proj.captions.titles[0];
        let yc = 0.2 * vh as f64; // 256
        let bbox = t.bbox.as_ref().unwrap();
        assert_eq!(bbox[0], (vw as f64 * 0.08) as i64); // 57
        assert_eq!(bbox[2], (vw as f64 * 0.92) as i64 - (vw as f64 * 0.08) as i64); // 662-57=605
        assert_eq!(bbox[1], (yc - vh as f64 * 0.045) as i64); // 256-57=199
    }

    // white-card fallback: sub_style без scene_color + solid светлый (lum>0.7) титр -> scene_color=#FFFFFF.
    #[test]
    fn white_card_activates_cover() {
        let vw = 720i64;
        let vh = 1280i64;
        let mut proj = Project::default();
        proj.captions.sub_style = Some(SubStyle::default()); // scene_color=None
        proj.raw_ctx.insert(
            "titles".into(),
            serde_json::json!([{"text": "T", "tgt": "Т", "y_frac": 0.1, "solid": true, "bg": "#FFFFFF"}]),
        );
        let paths = dummy_paths();
        let ctx = ComposeCtx {
            vw, vh, total: 30.0, do_translate: true, fresh_subs: false,
            src_lang: "en", paths: &paths,
        };
        let noop: Box<Progress> = Box::new(|_| {});
        run(&mut proj, &[], &[], &[], &ctx, &noop);
        let ss = proj.captions.sub_style.as_ref().unwrap();
        assert_eq!(ss.scene_color.as_deref(), Some("#FFFFFF"));
        assert!(ss.scene_flat);
    }

    // sub_px: медиана высот OCR-боксов на sub_y (±7%vh). Эталон: боксы h=[40,44,48] на sub_y=1050 -> 44.
    #[test]
    fn sub_px_median_on_band() {
        let vw = 720i64;
        let vh = 1280i64;
        let sub_y = 1050i64;
        let localize = vec![
            reg("a", 200, 1040, 100, 40, 0.0, 1.0),
            reg("b", 200, 1044, 100, 44, 1.0, 2.0),
            reg("c", 200, 1046, 100, 48, 2.0, 3.0),
            reg("d", 200, 200, 100, 30, 0.0, 1.0), // вне полосы
        ];
        let mut proj = Project::default();
        proj.captions.sub_y = Some(sub_y);
        let paths = dummy_paths();
        let ctx = ComposeCtx {
            vw, vh, total: 30.0, do_translate: true, fresh_subs: false,
            src_lang: "ru", paths: &paths,
        };
        let noop: Box<Progress> = Box::new(|_| {});
        run(&mut proj, &localize, &[], &[], &ctx, &noop);
        assert_eq!(proj.captions.raw_plan.get("sub_px").and_then(|v| v.as_i64()), Some(44));
    }
}
