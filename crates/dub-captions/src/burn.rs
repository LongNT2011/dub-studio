//! burn/burn_frame — ffmpeg + libass. Дословный порт captions.burn/burn_frame: полнокадровый gblur,
//! композит обратно ТОЛЬКО внутри каждого (плотного) текст-бокса, затем оверлей ASS. NVENC h264/hevc,
//! GPU-декод. burn_frame — ОДИН кадр в PNG (input-seek, без ре-энкода) для превью.

use crate::ass;
use crate::types::BlurBox;
use std::path::Path;
use std::process::Command;

#[cfg(windows)]
const FFMPEG: &str = "ffmpeg.exe";
#[cfg(not(windows))]
const FFMPEG: &str = "ffmpeg";

/// Экранировать путь ASS для filtergraph: \ -> /, : -> \: .
fn ass_escape(p: &Path) -> String {
    let abs = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let s = abs.to_string_lossy().to_string();
    // canonicalize на Windows даёт \\?\ префикс — убираем его, libass его не понимает.
    let s = s.strip_prefix(r"\\?\").map(|x| x.to_string()).unwrap_or(s);
    s.replace('\\', "/").replace(':', "\\:")
}

/// Собрать `ass='...':fontsdir='...'` фильтр (fontsdir указывает libass на bundled-шрифты).
fn ass_filter(ass_path: &Path) -> String {
    let ass = ass_escape(ass_path);
    let fd_dir = ass::fonts_dir();
    if fd_dir.exists() {
        let fd = ass_escape(&fd_dir);
        format!("ass='{ass}':fontsdir='{fd}'")
    } else {
        format!("ass='{ass}'")
    }
}

fn en(t0: f64, t1: f64) -> String {
    format!("enable='between(t\\,{:.2}\\,{:.2})'", t0, t1)
}

/// Собрать filter_complex графа блюр+ASS (общий для burn/burn_frame). `prefix_parts` — начальные
/// узлы (для burn_frame это select). Возвращает (graph, использует_ли_filter_complex).
fn blur_graph(
    ass_f: &str,
    blur_boxes: &[BlurBox],
    frame_w: i64,
    frame_h: i64,
    blur_sigma: i64,
    lead_parts: &[String],
    base_label: &str,
) -> String {
    let n = blur_boxes.len();
    let mut parts: Vec<String> = lead_parts.to_vec();
    parts.push(format!("[{base_label}]split=2[base][bsrc]"));
    parts.push(format!("[bsrc]gblur=sigma={}[blr]", blur_sigma));
    let srcs: Vec<String>;
    if n > 1 {
        let mut s = format!("[blr]split={n}");
        for i in 0..n {
            s.push_str(&format!("[s{i}]"));
        }
        parts.push(s);
        srcs = (0..n).map(|i| format!("s{i}")).collect();
    } else {
        srcs = vec!["blr".to_string()];
    }
    let mut cur = "base".to_string();
    for (i, bx) in blur_boxes.iter().enumerate() {
        let (dx, dy) = (2i64, 2i64);
        let bx0 = (bx.x - dx).max(0);
        let by0 = (bx.y - dy).max(0);
        let bw = (bx.w + 2 * dx).min(frame_w - bx0);
        let bh = (bx.h + 2 * dy).min(frame_h - by0);
        let t0b = (bx.t0 - 0.6).max(0.0);
        let t1b = bx.t1 + 0.4;
        parts.push(format!("[{}]crop={bw}:{bh}:{bx0}:{by0}[c{i}]", srcs[i]));
        parts.push(format!("[{cur}][c{i}]overlay={bx0}:{by0}:{}[v{i}]", en(t0b, t1b)));
        cur = format!("v{i}");
    }
    format!("{};[{cur}]{ass_f}[outv]", parts.join(";"))
}

/// Вжечь блюр боксов оригинала + оверлей ASS. blur_boxes: [(x,y,w,h,t0,t1)]. Без аудио (муксится
/// отдельно). NVENC only (нет тихого CPU-фолбэка). Порт captions.burn.
#[allow(clippy::too_many_arguments)]
pub fn burn(
    video: &Path,
    ass_path: &Path,
    out: &Path,
    blur_boxes: &[BlurBox],
    frame_size: Option<(i64, i64)>,
    blur: bool,
    gpu_encode: bool,
    gpu_decode: bool,
    cq: i64,
    src_codec: Option<&str>,
    blur_sigma: i64,
) -> Result<(), String> {
    let ass_f = ass_filter(ass_path);
    let (w, h) = frame_size.unwrap_or((1_000_000_000, 1_000_000_000));
    let hevc = matches!(src_codec.map(|c| c.to_lowercase()).as_deref(), Some("hevc") | Some("h265"));
    let nv: Vec<String> = vec![
        "-c:v".into(),
        (if hevc { "hevc_nvenc" } else { "h264_nvenc" }).into(),
        "-preset".into(),
        "p4".into(),
        "-cq".into(),
        cq.to_string(),
    ];
    let sw: Vec<String> = vec![
        "-c:v".into(),
        (if hevc { "libx265" } else { "libx264" }).into(),
        "-preset".into(),
        "medium".into(),
        "-crf".into(),
        (cq - 2).max(0).to_string(),
    ];

    let vargs: Vec<String> = if !blur_boxes.is_empty() && blur {
        let graph = blur_graph(&ass_f, blur_boxes, w, h, blur_sigma, &[], "0:v");
        vec!["-filter_complex".into(), graph, "-map".into(), "[outv]".into()]
    } else {
        vec!["-vf".into(), ass_f.clone()]
    };

    let enc = if gpu_encode { nv } else { sw };
    run_ffmpeg(video, &vargs, &enc, out, gpu_decode)
}

fn run_ffmpeg(
    video: &Path,
    vargs: &[String],
    enc: &[String],
    out: &Path,
    gpu_decode: bool,
) -> Result<(), String> {
    let mut cmd = Command::new(FFMPEG);
    cmd.arg("-y");
    if gpu_decode {
        cmd.args(["-hwaccel", "cuda"]);
    }
    cmd.arg("-i").arg(video).arg("-an");
    for a in vargs {
        cmd.arg(a);
    }
    for a in enc {
        cmd.arg(a);
    }
    cmd.arg(out);
    let o = cmd.output().map_err(|e| format!("ffmpeg запуск: {e}"))?;
    if !o.status.success() {
        let err = String::from_utf8_lossy(&o.stderr);
        let tail: String = err.chars().rev().take(1000).collect::<String>().chars().rev().collect();
        return Err(format!("ffmpeg caption burn failed:\n{tail}"));
    }
    Ok(())
}

/// ОДИН превью-кадр в момент t: тот же блюр+ASS граф, но input-seek + декод одного кадра в PNG. Порт
/// captions.burn_frame.
#[allow(clippy::too_many_arguments)]
pub fn burn_frame(
    video: &Path,
    ass_path: &Path,
    out_png: &Path,
    t: f64,
    blur_boxes: &[BlurBox],
    frame_size: Option<(i64, i64)>,
    blur: bool,
    blur_sigma: i64,
) -> Result<(), String> {
    let t = t.max(0.0);
    let ass_f = ass_filter(ass_path);
    let (w, h) = frame_size.unwrap_or((1_000_000_000, 1_000_000_000));
    let sel = format!("select='gte(t\\,{:.3})'", t);

    let vargs: Vec<String> = if !blur_boxes.is_empty() && blur {
        // lead: select -> [sel]; base label = sel.
        let lead = vec![format!("[0:v]{sel}[sel]")];
        let graph = blur_graph(&ass_f, blur_boxes, w, h, blur_sigma, &lead, "sel");
        vec!["-filter_complex".into(), graph, "-map".into(), "[outv]".into()]
    } else {
        vec!["-vf".into(), format!("{sel},{ass_f}")]
    };

    let mut cmd = Command::new(FFMPEG);
    cmd.arg("-y")
        .arg("-ss")
        .arg(format!("{:.3}", (t - 1.0).max(0.0)))
        .arg("-copyts")
        .arg("-i")
        .arg(video)
        .arg("-an");
    for a in &vargs {
        cmd.arg(a);
    }
    cmd.args(["-frames:v", "1", "-update", "1"]).arg(out_png);
    let o = cmd.output().map_err(|e| format!("ffmpeg запуск: {e}"))?;
    if !o.status.success() {
        let err = String::from_utf8_lossy(&o.stderr);
        let tail: String = err.chars().rev().take(1000).collect::<String>().chars().rev().collect();
        return Err(format!("ffmpeg preview frame failed:\n{tail}"));
    }
    Ok(())
}
