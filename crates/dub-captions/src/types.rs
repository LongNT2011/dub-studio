//! Входные типы для build/burn. Легковесные структуры, которые рендер-стадия заполняет из Project
//! (dub-core). Держим их отдельно, чтобы dub-captions не зависел от dub-core (чистый рендер-крейт).

/// Титр (localized loc_block): текст рисуется В МЕСТЕ бокса. Порт полей Title из project.py, что
/// использует _emit_title (bbox/color/bg/font/italic/align/start/end/lh/solid/bold/size_px/outline/…).
#[derive(Clone, Debug, Default)]
pub struct Title {
    pub text: String,
    pub bbox: Option<Vec<i64>>,
    pub color: Option<String>,
    pub bg: Option<String>,
    pub font: Option<String>,
    pub italic: bool,
    pub align: String,
    pub start: f64,
    pub end: f64,
    pub lh: Option<i64>,
    pub solid: bool,
    pub bold: bool,
    pub size_px: Option<i64>,
    pub outline: Option<String>,
    pub outline_w: Option<i64>,
    pub uppercase: bool,
}

/// Стиль субтитра (sub_style от vision-оркестратора). Порт SubStyle из project.py.
#[derive(Clone, Debug)]
pub struct SubStyle {
    pub color: String,
    pub background: Option<String>,
    pub outline: Option<String>,
    pub outline_w: Option<i64>,
    pub bold: bool,
    pub italic: bool,
    pub uppercase: bool,
    pub align: String,
    pub font: Option<String>,
    pub n_lines: Option<i64>,
    pub size_frac: Option<f64>,
    pub size_px: Option<i64>,
    pub scene_color: Option<String>,
    pub scene_flat: bool,
    pub solid: bool,
}

impl Default for SubStyle {
    fn default() -> Self {
        SubStyle {
            color: "#FFFFFF".to_string(),
            background: None,
            outline: None,
            outline_w: None,
            bold: false,
            italic: false,
            uppercase: false,
            align: "center".to_string(),
            font: None,
            n_lines: None,
            size_frac: None,
            size_px: None,
            scene_color: None,
            scene_flat: false,
            solid: false,
        }
    }
}

/// Один субтитр-сегмент для build: start/end/tgt (+ опц. y, куда поставить строку).
#[derive(Clone, Debug, Default)]
pub struct Sub {
    pub start: f64,
    pub end: f64,
    pub tgt: String,
    pub y: Option<i64>,
}

/// Blur-бокс для burn: (x,y,w,h,t0,t1).
#[derive(Clone, Copy, Debug, Default)]
pub struct BlurBox {
    pub x: i64,
    pub y: i64,
    pub w: i64,
    pub h: i64,
    pub t0: f64,
    pub t1: f64,
}
