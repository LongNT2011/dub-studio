//! Динамический резолв активного варианта модели для КАЖДОГО движка — переключение квантов.
//!
//! Порт-баг, который это чинит: пути моделей морозились в AppState при старте, а `set_opts` был
//! заглушкой — скачанный альт-квант (Higgs q6_k, Roformer Q5_0, Parakeet fp32, Gemma q8_0) никогда
//! не применялся, работали только дефолты. Теперь резолв идёт ПРИ КАЖДОЙ джобе (analyze/render):
//!   env-override → сохранённый выбор (models/active.json) → скан установленного → дефолт.
//! Так «скачал/выбрал квант → применился» без рестарта сервера.
//!
//! active.json = {"tts":"q6_k","asr":"fp32","mt":"q8_0","sep":"Q5_0"} — токен варианта на движок.
//! Пишется при завершении скачки компонента (setup) и при смене в дропдауне (POST /engine/select).

use serde_json::Value;
use std::path::{Path, PathBuf};

/// Прочитать сохранённый выбор вариантов (models/active.json). Нет файла/битый → пусто (=авто).
pub fn load_selection(mroot: &Path) -> Value {
    std::fs::read_to_string(mroot.join("active.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| Value::Object(Default::default()))
}

/// Записать/обновить один слот выбора (engine -> variant) атомарно.
pub fn set_selection(mroot: &Path, engine: &str, variant: &str) -> std::io::Result<()> {
    let mut v = load_selection(mroot);
    v.as_object_mut()
        .expect("load_selection returns object")
        .insert(engine.to_string(), Value::String(variant.to_string()));
    let _ = std::fs::create_dir_all(mroot);
    let tmp = mroot.join("active.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&v).unwrap_or_default())?;
    std::fs::rename(&tmp, mroot.join("active.json"))
}

fn pick<'a>(sel: &'a Value, engine: &str) -> Option<&'a str> {
    sel.get(engine).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

/// Отобразить id компонента манифеста -> (engine, variant-токен) для записи выбора при скачивании.
/// None — компонент не является переключаемым вариантом модели (движок/рантайм/OCR и т.п.).
pub fn component_selection(id: &str) -> Option<(&'static str, String)> {
    Some(match id {
        "higgs" => ("tts", "q8_0".into()),
        "higgs-q6_k" => ("tts", "q6_k".into()),
        "higgs-q4_k_m" => ("tts", "q4_k_m".into()),
        "parakeet" => ("asr", "int8".into()),
        "parakeet-fp32" => ("asr", "fp32".into()),
        "gemma" => ("mt", "q4_0".into()),
        "gemma-q5_0" => ("mt", "q5_0".into()),
        "gemma-q6_k" => ("mt", "q6_k".into()),
        "gemma-q8_0" => ("mt", "q8_0".into()),
        "roformer" => ("sep", "Q8_0".into()),
        "roformer-q5" => ("sep", "Q5_0".into()),
        "roformer-q4" => ("sep", "Q4_0".into()),
        _ => return None,
    })
}

/// Higgs TTS: папки higgs-{q8_0,q6_k,q4_k_m}, внутри файл {q}.gguf. Возврат (каталог, квант-строка
/// для audiocpp load_model). Env DUB_STUDIO_HIGGS_MODEL (портатив) имеет приоритет.
pub fn resolve_tts(mroot: &Path, sel: &Value) -> (PathBuf, String) {
    if let Ok(env) = std::env::var("DUB_STUDIO_HIGGS_MODEL") {
        let d = PathBuf::from(env);
        let q = d
            .file_name()
            .and_then(|s| s.to_str())
            .and_then(|n| n.strip_prefix("higgs-"))
            .unwrap_or("q8_0")
            .to_string();
        return (d, q);
    }
    let has = |q: &str| mroot.join(format!("higgs-{q}")).join(format!("{q}.gguf")).is_file();
    let ret = |q: &str| (mroot.join(format!("higgs-{q}")), q.to_string());
    if let Some(q) = pick(sel, "tts") {
        if has(q) {
            return ret(q);
        }
    }
    for q in ["q8_0", "q6_k", "q4_k_m"] {
        if has(q) {
            return ret(q);
        }
    }
    ret("q8_0") // дефолт-fallback (может ещё не быть скачан)
}

/// Roformer сепарация: models/bsroformer/voc_fv6-{Q8_0,Q5_0,Q4_0}.gguf (все в одном каталоге).
/// Env DUB_STUDIO_BSROFORMER_MODEL имеет приоритет.
pub fn resolve_sep(mroot: &Path, sel: &Value) -> PathBuf {
    if let Ok(env) = std::env::var("DUB_STUDIO_BSROFORMER_MODEL") {
        return PathBuf::from(env);
    }
    let f = |q: &str| mroot.join("bsroformer").join(format!("voc_fv6-{q}.gguf"));
    if let Some(q) = pick(sel, "sep") {
        let p = f(q);
        if p.is_file() {
            return p;
        }
    }
    for q in ["Q8_0", "Q5_0", "Q4_0"] {
        let p = f(q);
        if p.is_file() {
            return p;
        }
    }
    f("Q8_0")
}

/// Parakeet ASR: каталоги tdt (int8) / tdt-fp32. from_pretrained сам различает имена файлов внутри.
/// Env DUB_STUDIO_TDT имеет приоритет.
pub fn resolve_asr(mroot: &Path, sel: &Value) -> PathBuf {
    if let Ok(env) = std::env::var("DUB_STUDIO_TDT") {
        return PathBuf::from(env);
    }
    let fp32 = mroot.join("tdt-fp32");
    let int8 = mroot.join("tdt");
    let fp32_ok = fp32.join("encoder-model.onnx").is_file();
    let int8_ok = int8.join("encoder-model.int8.onnx").is_file();
    match pick(sel, "asr") {
        Some("fp32") if fp32_ok => fp32,
        Some("int8") if int8_ok => int8,
        _ if int8_ok => int8,
        _ if fp32_ok => fp32,
        _ => int8,
    }
}

/// Gemma MT + vision: папки mt-q8_0/mt-q6_k/mt-q5_0 + mt (q4_0-дефолт). Возврат (модель, mmproj).
/// Каталог годится, только если есть И модель, И mmproj (полускачанный игнорируется). Имя файла не
/// важно — берём любой .gguf (mmproj по подстроке). Env-root уже учтён в mroot.
pub fn resolve_mt(mroot: &Path, sel: &Value) -> (PathBuf, PathBuf) {
    let dir_for = |q: &str| if q == "q4_0" { mroot.join("mt") } else { mroot.join(format!("mt-{q}")) };
    let find = |dir: &Path, want_mmproj: bool| -> Option<PathBuf> {
        let mut hit: Option<PathBuf> = None;
        for e in std::fs::read_dir(dir).ok()?.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) != Some("gguf") {
                continue;
            }
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("").to_lowercase();
            if name.contains("mmproj") == want_mmproj
                && hit.as_ref().map(|h| p < *h).unwrap_or(true)
            {
                hit = Some(p);
            }
        }
        hit
    };
    let try_dir = |q: &str| {
        let d = dir_for(q);
        match (find(&d, false), find(&d, true)) {
            (Some(m), Some(mm)) => Some((m, mm)),
            _ => None,
        }
    };
    if let Some(q) = pick(sel, "mt") {
        if let Some(r) = try_dir(q) {
            return r;
        }
    }
    for q in ["q8_0", "q6_k", "q5_0", "q4_0"] {
        if let Some(r) = try_dir(q) {
            return r;
        }
    }
    (
        mroot.join("mt").join("gemma-4-12b-it-qat-q4_0.gguf"),
        mroot.join("mt").join("mmproj-gemma-4-12b-it-qat-q4_0.gguf"),
    )
}
