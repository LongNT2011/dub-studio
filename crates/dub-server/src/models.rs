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

/// Отобразить id компонента манифеста -> список (slot, значение) для записи выбора при скачивании.
/// Пусто — компонент не является переключаемым вариантом модели (движок/рантайм/OCR и т.п.).
/// ASR-варианты пишут ДВА слота: движок (asr_engine) + вариант этого движка (asr-квант / whisper-модель),
/// чтобы скачивание Whisper-модели сразу делало Whisper активным движком (и наоборот для Parakeet).
pub fn component_selection(id: &str) -> Vec<(&'static str, String)> {
    match id {
        "higgs" => vec![("tts", "q8_0".into())],
        "higgs-q6_k" => vec![("tts", "q6_k".into())],
        "higgs-q4_k_m" => vec![("tts", "q4_k_m".into())],
        "parakeet" => vec![("asr_engine", "parakeet".into()), ("asr", "int8".into())],
        "parakeet-fp32" => vec![("asr_engine", "parakeet".into()), ("asr", "fp32".into())],
        "whisper-tiny" => vec![("asr_engine", "whisper".into()), ("whisper_model", "tiny".into())],
        "whisper-base" => vec![("asr_engine", "whisper".into()), ("whisper_model", "base".into())],
        "whisper-small" => vec![("asr_engine", "whisper".into()), ("whisper_model", "small".into())],
        "whisper-medium" => vec![("asr_engine", "whisper".into()), ("whisper_model", "medium".into())],
        "whisper-large-v3" => vec![("asr_engine", "whisper".into()), ("whisper_model", "large-v3".into())],
        "whisper-large-v3-turbo" => {
            vec![("asr_engine", "whisper".into()), ("whisper_model", "large-v3-turbo".into())]
        }
        "gemma" => vec![("mt", "q4_0".into())],
        "gemma-q5_0" => vec![("mt", "q5_0".into())],
        "gemma-q6_k" => vec![("mt", "q6_k".into())],
        "gemma-q8_0" => vec![("mt", "q8_0".into())],
        "roformer" => vec![("sep", "Q8_0".into())],
        "roformer-q5" => vec![("sep", "Q5_0".into())],
        "roformer-q4" => vec![("sep", "Q4_0".into())],
        _ => vec![],
    }
}

/// Разрешённые слоты для прямой установки через POST /engine/select {key,value} (без скачивания):
/// переключение движка/модели/кванта + видимые в настройках лимиты RAM. Возврат true, если слот допустим.
pub fn is_selection_key(key: &str) -> bool {
    matches!(
        key,
        "tts" | "asr" | "mt" | "sep" | "asr_engine" | "whisper_model" | "whisper_compute" | "whisper_device"
            // Лимиты RAM (видимые контролы в настройках, НЕ авто-магия): против OOM на слабой памяти.
            | "llama_ubatch"    // размер prefill-батча Gemma (меньше = меньше пиковый буфер графа prefill)
            | "higgs_ref_secs"  // длина реф-клипа клона голоса (меньше = меньше prefill Higgs; <12с спасает 32ГБ)
            | "bench"           // пер-стадийный бенчмарк (bench.json + ⏱ в журнале); галка в настройках, ВЫКЛ по умолчанию
    )
}

/// Включён ли пер-стадийный бенчмарк (галка в настройках -> active.json "bench"="1"). По умолчанию ВЫКЛ:
/// фоновый семплер NVML/sysinfo и bench.json нужны только для сравнения настроек, не в обычной работе.
pub fn bench_enabled(mroot: &Path) -> bool {
    pick(&load_selection(mroot), "bench") == Some("1")
}

/// Прочитать числовой слот выбора (llama_ubatch/higgs_ref_secs) из active.json. Значение может лежать
/// строкой ("256") или числом — обе формы принимаем. None/пусто -> дефолт у вызывающего.
pub fn sel_num(mroot: &Path, key: &str) -> Option<f64> {
    let v = load_selection(mroot);
    match v.get(key) {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

/// Длина реф-клипа клона голоса в секундах: настройка higgs_ref_secs (видимая в UI), дефолт 12.0.
/// Пользователь на 32ГБ RAM может уменьшить (баг-репорт: >12с не влезает в prefill Higgs, ручная резка <12с спасает).
pub fn higgs_ref_secs(mroot: &Path) -> f64 {
    sel_num(mroot, "higgs_ref_secs").filter(|s| *s > 0.0 && *s <= 60.0).unwrap_or(12.0)
}

/// Выбор ASR-движка для одной джобы: Parakeet (каталог TDT) либо Whisper (бинарь + модель + квант + девайс).
#[derive(Debug, Clone)]
pub enum AsrChoice {
    Parakeet(PathBuf),
    Whisper { bin: PathBuf, model_dir: PathBuf, model: String, compute: String, device: String },
}

impl AsrChoice {
    /// Строка для лога `[models]` — видно, каким движком реально пойдёт транскрипция.
    /// ВАЖНО: участвует в param_hash ASR-стадии (analyze.rs) — только СТАБИЛЬНЫЕ токены (вариант
    /// каталога, не абсолютный путь), иначе кэш/чекпоинты инвалидируются от переноса репо между
    /// машинами/папками (ревью-находка).
    pub fn describe(&self) -> String {
        match self {
            AsrChoice::Parakeet(d) => {
                let variant = d.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "tdt".into());
                format!("Parakeet ({variant})")
            }
            AsrChoice::Whisper { model, compute, device, .. } => {
                format!("Whisper {model} (compute={compute}, device={device})")
            }
        }
    }
}

/// Путь к бинарю Whisper: env DUB_STUDIO_WHISPER_BIN, иначе приоритетом XXL-сборка
/// (<repo>/tools/whisper/Faster-Whisper-XXL/faster-whisper-xxl.exe — свежий движок, CUDA-DLL в
/// комплекте (_xxl_data), умеет --batched), фолбэк — старый onefile whisper-faster.exe (CPU).
pub fn whisper_bin(repo_root: &Path) -> PathBuf {
    if let Ok(p) = std::env::var("DUB_STUDIO_WHISPER_BIN") {
        return PathBuf::from(p);
    }
    let wdir = repo_root.join("tools").join("whisper");
    if cfg!(windows) {
        let xxl = wdir.join("Faster-Whisper-XXL").join("faster-whisper-xxl.exe");
        if xxl.is_file() {
            return xxl;
        }
        return wdir.join("whisper-faster.exe");
    }
    wdir.join("whisper-faster")
}

/// Каталог Whisper-моделей: <mroot>/whisper (внутри — faster-whisper-<size>). Есть ли модель на диске.
fn whisper_model_installed(mroot: &Path, size: &str) -> bool {
    mroot.join("whisper").join(format!("faster-whisper-{size}")).join("model.bin").is_file()
}

/// Резолв активного ASR: если выбран движок whisper И бинарь+модель на диске — Whisper (модель = выбор,
/// иначе первый установленный по убыванию качества); иначе — Parakeet (существующий резолв каталога TDT).
/// Так «выбрал Whisper + скачал модель» применяется без рестарта, а недо-настроенный Whisper тихо
/// откатывается на Parakeet (analyze не падает).
pub fn resolve_asr_choice(repo_root: &Path, mroot: &Path, sel: &Value) -> AsrChoice {
    if pick(sel, "asr_engine") == Some("whisper") {
        let bin = whisper_bin(repo_root);
        // выбранная модель, если скачана; иначе — лучшая из установленных.
        let want = pick(sel, "whisper_model").filter(|m| whisper_model_installed(mroot, m));
        let model = want.map(String::from).or_else(|| {
            ["large-v3-turbo", "large-v3", "medium", "small", "base", "tiny"]
                .into_iter()
                .find(|m| whisper_model_installed(mroot, m))
                .map(String::from)
        });
        if let (true, Some(model)) = (bin.is_file(), model) {
            // Девайс Whisper: авто по ФАКТУ наличия CUDA-либ. whisper-faster (CTranslate2, CUDA 11)
            // требует cublas64_11 + cudnn8 РЯДОМ С EXE (официальный Purfview: GPU execution requires
            // cuBLAS and cuDNN libs next to the executable). CUDA-13 DLL Higgs'а ему не подходят —
            // имена версионные (cublas64_13 ≠ cublas64_11), cuDNN в дистрибутиве нет вообще. Поэтому:
            // либы лежат -> cuda (GPU в разы быстрее на длинных), нет -> честный cpu БЕЗ попыток и
            // фолбэков. Явная настройка whisper_device перекрывает авто-детект.
            let auto_dev = if whisper_cuda_libs_present(&bin) { "cuda" } else { "cpu" };
            let device = pick(sel, "whisper_device").unwrap_or(auto_dev).to_string();
            // Квант: на GPU дефолт float16 (родной для тензорных ядер), на CPU — int8.
            let mut compute = pick(sel, "whisper_compute")
                .unwrap_or(if device == "cuda" { "float16" } else { "int8" })
                .to_string();
            // ГАРД: float16/bfloat16 не поддерживаются на CPU — CTranslate2 роняет процесс с
            // "Requested float16 compute type, but the target device do not support efficient float16".
            // На cpu коэрсим GPU-only кванты в безопасный int8, чтобы транскрипция не падала.
            if device == "cpu"
                && matches!(compute.as_str(), "float16" | "bfloat16" | "int8_float16" | "int8_bfloat16")
            {
                compute = "int8".to_string();
            }
            return AsrChoice::Whisper { bin, model_dir: mroot.join("whisper"), model, compute, device };
        }
    }
    AsrChoice::Parakeet(resolve_asr(mroot, sel))
}

/// Лежат ли рядом с whisper-faster.exe CUDA-библиотеки CTranslate2 (cuBLAS 11/12 + cuDNN).
/// Ровно те имена, что требует движок; без них cuda-запуск гарантированно падает.
/// XXL-сборка — self-contained: cublas64_12 + cudnn64_8 лежат внутри её _xxl_data (проверено по
/// содержимому архива r245.4) -> для неё сразу true.
fn whisper_cuda_libs_present(bin: &std::path::Path) -> bool {
    if bin
        .file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|n| n.eq_ignore_ascii_case("faster-whisper-xxl.exe"))
    {
        return true;
    }
    let Some(dir) = bin.parent() else { return false };
    let cublas = dir.join("cublas64_11.dll").is_file() || dir.join("cublas64_12.dll").is_file();
    let cudnn = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten().any(|e| {
                let n = e.file_name().to_string_lossy().to_lowercase();
                n.starts_with("cudnn") && n.ends_with(".dll")
            })
        })
        .unwrap_or(false);
    cublas && cudnn
}

/// Построить ASR-движок из выбора (boxed trait-object): analyze не знает деталей резолва.
pub fn build_engine(choice: &AsrChoice) -> Box<dyn dub_asr::AsrEngine> {
    match choice {
        AsrChoice::Parakeet(dir) => Box::new(dub_asr::Asr::new(dir)),
        AsrChoice::Whisper { bin, model_dir, model, compute, device } => {
            Box::new(dub_asr::WhisperAsr::new(bin, model_dir, model, compute, device))
        }
    }
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

#[cfg(test)]
mod resolve_live_tests {
    use super::*;

    // Диагностический (марафон QC): резолв ASR на живых путях репо. На CI без models/ — скип.
    #[test]
    fn resolve_asr_choice_on_live_repo() {
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent().unwrap().parent().unwrap();
        let mroot = repo.join("models");
        if !mroot.join("active.json").is_file() {
            eprintln!("skip: нет models/active.json");
            return;
        }
        let sel = load_selection(&mroot);
        let choice = resolve_asr_choice(repo, &mroot, &sel);
        eprintln!("sel = {sel}");
        eprintln!("resolved = {}", choice.describe());
        // Ассертим Whisper только когда он РЕАЛЬНО установлен: резолв по контракту тихо откатывается
        // на Parakeet без бинаря/модели (ревью: иначе тест ложно валится на машине без whisper).
        let whisper_ready = whisper_bin(repo).is_file()
            && ["large-v3-turbo", "large-v3", "medium", "small", "base", "tiny"]
                .iter()
                .any(|m| whisper_model_installed(&mroot, m));
        if pick(&sel, "asr_engine") == Some("whisper") && whisper_ready {
            assert!(
                choice.describe().starts_with("Whisper"),
                "active.json просит whisper (и он установлен), но резолв дал: {}", choice.describe()
            );
        }
    }
}
