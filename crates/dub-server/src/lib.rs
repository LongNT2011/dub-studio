//! dub-server — axum-реализация REST/SSE контракта Dub Studio. Цель: SPA (frontend/dist) работает
//! без правок против этого сервера так же, как против backend/app.py.
//!
//! Раунд 1 реализует: раздачу SPA с защитой от traversal, GET /engine/capabilities, POST /projects
//! (multipart-загрузка видео -> workspace/<pid>/), GET /projects/{id}, каркас очереди джоб + SSE
//! GET /jobs/{id}/events. GPU-эндпоинты (analyze/render/preview/patch и т.д.) — каркас на следующие
//! раунды; их карта в docs/PORT-CONTRACT.md.

mod analyze;
mod compose;
mod endpoints;
mod frame;
mod hw;
mod jobs;
mod media;
mod models;
mod ocr;
mod patch;
mod record;
mod render;
mod setup;
mod spa;
mod translate;
mod wavio;

use axum::extract::{Multipart, Path as AxPath, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use dub_core::{EngineOpts, Project};
use futures_util::stream::Stream;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use jobs::JobQueue;

/// E2E-верификация caption-композита БЕЗ ASR/Gemma/TTS: берём УЖЕ проанализированный project.json
/// (кэш transcript+raw_ctx), заново гоним OCR-стадию + compose (реальная детекция + матчинг титров),
/// затем вжигаем блюр+сабы в captioned.mp4. Проверка на живых данных, не
/// перезапуская тяжёлый Gemma-vision. Возвращает путь к captioned.mp4.
pub fn verify_captions_e2e(
    repo_root: &Path,
    project_json: &Path,
    input_video: &Path,
    work_dir: &Path,
) -> Result<PathBuf, String> {
    let text = std::fs::read_to_string(project_json).map_err(|e| e.to_string())?;
    let mut proj = Project::from_json(&text).map_err(|e| e.to_string())?;

    // OCR-стадия перезаписывает captions.blur_boxes/titles/sub_style/sub_px через compose. Чистим
    // предыдущий вывод, чтобы не смешивать со старым blur из project.json.
    proj.captions.blur_boxes.clear();
    proj.captions.titles.clear();

    let vw = proj.meta.width;
    let vh = proj.meta.height;
    let total = proj.meta.duration;
    let src_codec = proj.meta.src_codec.clone();

    // Пути резолвим напрямую из repo_root (без AppState — тот тянет Tokio-JobQueue). ocr::stage нужны
    // лишь models_root/caption_fps/input/work_dir; llama_bin/mt_model — только для таглайн-MT (fail-safe).
    let opts = EngineOpts::default();
    let mroot = models_root(repo_root);
    let fonts_dir = repo_root.join("fonts");
    let unused = repo_root.join("nonexistent");
    let args = analyze::AnalyzeArgs {
        tgt_lang: proj.tgt_lang.clone(),
        mode: proj.mode.clone(),
        src_lang: "auto".into(),
        subs: proj.subs.mode.clone(),
        rewrite: String::new(),
        burn: proj.subs.burn,
        detect_text: true,
    };
    let sel = models::load_selection(&mroot);
    let (mt_model, mmproj) = models::resolve_mt(&mroot, &sel);
    let paths = analyze::AnalyzePaths {
        input: input_video.to_path_buf(),
        work_dir: work_dir.to_path_buf(),
        asr: models::AsrChoice::Parakeet(unused.clone()), // ocr::stage не трогает ASR (verify-путь)
        sortformer_onnx: unused.clone(),
        llama_bin: dub_llm::resolve_llama_bin(&repo_root.join("tools").join("llama")),
        mt_model,
        mmproj,
        models_root: mroot,
        caption_fps: opts.caption_fps,
    };
    let cb = |ev: Value| {
        if let Some(m) = ev.get("msg").and_then(|v| v.as_str()) {
            eprintln!("[verify] {m}");
        }
    };
    ocr::stage(&args, &paths, &mut proj, vw, vh, total, &cb);

    let out_ass = work_dir.join("verify_caps.ass");
    let captioned = work_dir.join("verify_captioned.mp4");
    render::build_and_burn_captions(
        &proj, input_video, &out_ass, &captioned, &fonts_dir, vw, vh, total, &src_codec,
    )?;
    // Записать обновлённый project (с bbox титров) рядом — для инспекции.
    let dbg = work_dir.join("verify_project.json");
    let _ = std::fs::write(&dbg, proj.to_json_pretty().unwrap_or_default());
    Ok(captioned)
}

#[derive(Clone)]
pub struct AppState {
    pub repo_root: PathBuf,
    pub workspace: PathBuf,
    pub web_root: Option<PathBuf>,
    pub opts: Arc<EngineOpts>,
    pub jobs: JobQueue,
    /// Каталог TDT-модели ASR (analyze). Env DUB_STUDIO_TDT, иначе <models_root>/tdt.
    pub tdt_dir: PathBuf,
    /// Путь к Sortformer .onnx (диаризация). Env DUB_STUDIO_SORTFORMER, иначе <models_root>/sortformer/…v2.onnx.
    pub sortformer_onnx: PathBuf,
    /// llama-server(.exe) — сайдкар перевода/vision. Env DUB_STUDIO_LLAMA_BIN, иначе <repo>/tools/llama/llama-server(.exe).
    pub llama_bin: PathBuf,
    /// BSRoformer CLI (сепарация). Env DUB_STUDIO_BSROFORMER_DIR/<cli>, иначе <repo>/tools/bsroformer/bs_roformer-cli.exe.
    pub bsroformer_cli: PathBuf,
    /// GGUF модель сепарации. Env DUB_STUDIO_BSROFORMER_MODEL, иначе <repo>/models/bsroformer/voc_fv6-Q8_0.gguf.
    pub bsroformer_model: PathBuf,
    /// Higgs audiocpp_engine.dll (TTS). Env DUB_STUDIO_HIGGS_DLL, иначе <models>/higgs-engine/audiocpp_engine.dll.
    pub higgs_dll: PathBuf,
    /// Каталог весов Higgs (q8_0). Env DUB_STUDIO_HIGGS_MODEL, иначе <models>/higgs-q8_0.
    pub higgs_model_root: PathBuf,
    /// Каталог bundled-шрифтов субтитров. Env DUB_STUDIO_FONTS_DIR, иначе <repo>/fonts.
    pub fonts_dir: PathBuf,
    /// Корень моделей (для OCR: <root>/ocr/…). Env DUBENGINE_MODELS_ROOT, иначе <repo>/models.
    pub models_root: PathBuf,
    /// Каталог голосов-паков + записей с микрофона. Env DUBENGINE_VOICES, иначе <repo>/voices.
    pub voices_dir: PathBuf,
    /// Флаг отмены текущей setup-закачки (POST /setup/cancel взводит; download-loop его читает).
    pub setup_cancel: Arc<std::sync::atomic::AtomicBool>,
}

/// Корень моделей: env DUBENGINE_MODELS_ROOT, иначе <repo_root>/models.
pub(crate) fn models_root(repo_root: &Path) -> PathBuf {
    std::env::var("DUBENGINE_MODELS_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| repo_root.join("models"))
}

/// Прописать в PATH процесса каталоги скачанных бинарей (ffmpeg, llama, движок Higgs с CUDA/VC-DLL),
/// чтобы `Command::new("ffmpeg")` и подобные находили их без перезапуска после автозакачки. Вызывается
/// один раз при старте сервера (main.rs / Tauri-shell). Идемпотентно: не дублирует уже присутствующие.
pub fn augment_path_for_tools(repo_root: &Path) {
    let dirs = [
        repo_root.join("tools").join("ffmpeg"),
        repo_root.join("tools").join("llama"),
        repo_root.join("models").join("higgs-engine"),
    ];
    let sep = if cfg!(windows) { ";" } else { ":" };
    let cur = std::env::var("PATH").unwrap_or_default();
    let mut prefix = String::new();
    for d in dirs {
        let s = d.to_string_lossy().to_string();
        if !s.is_empty() && !cur.split(sep).any(|p| p == s) && !prefix.split(sep).any(|p| p == s) {
            if !prefix.is_empty() {
                prefix.push_str(sep);
            }
            prefix.push_str(&s);
        }
    }
    if !prefix.is_empty() {
        std::env::set_var("PATH", format!("{prefix}{sep}{cur}"));
    }
}

/// Поднять axum-сервер БЛОКИРУЮЩЕ на собственном tokio-рантайме. Для встраивания в десктоп-оболочку ОДНИМ
/// процессом (вместо запуска dub-server.exe отдельным subprocess) — вызывать из фонового std::thread.
/// Слушает 127.0.0.1:port; augment PATH под инструменты делается здесь же.
pub fn serve_blocking(repo_root: impl AsRef<Path>, port: u16) -> anyhow::Result<()> {
    let root = repo_root.as_ref().to_path_buf();
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        augment_path_for_tools(&root);
        let state = AppState::new(&root);
        let app = build_router(state);
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, app).await?;
        Ok::<(), anyhow::Error>(())
    })
}

impl AppState {
    pub fn new(repo_root: impl AsRef<Path>) -> Self {
        let repo_root = repo_root.as_ref().to_path_buf();
        let workspace = repo_root.join("workspace");
        let _ = std::fs::create_dir_all(&workspace);
        let web_root = spa::find_web_root(&repo_root);
        let mroot = models_root(&repo_root);
        let tdt_dir = std::env::var("DUB_STUDIO_TDT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| mroot.join("tdt"));
        let sortformer_onnx = std::env::var("DUB_STUDIO_SORTFORMER")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                mroot
                    .join("sortformer")
                    .join("diar_streaming_sortformer_4spk-v2.onnx")
            });
        // llama-server: env-override, иначе <repo>/tools/llama/llama-server(.exe) (негитуемый каталог,
        // кладёт установщик/раунд 3). Существование проверяет сама стадия перевода (fail-safe).
        let llama_bin = dub_llm::resolve_llama_bin(&repo_root.join("tools").join("llama"));
        let bsroformer_cli = std::env::var("DUB_STUDIO_BSROFORMER_DIR")
            .map(|d| PathBuf::from(d).join(dub_sep::ENGINE_CLI_FILE))
            .unwrap_or_else(|_| dub_sep::engine_dir(&repo_root).join(dub_sep::ENGINE_CLI_FILE));
        let bsroformer_model = dub_sep::model_path(&repo_root);
        let higgs_dll = std::env::var("DUB_STUDIO_HIGGS_DLL")
            .map(PathBuf::from)
            .unwrap_or_else(|_| mroot.join("higgs-engine").join("audiocpp_engine.dll"));
        let higgs_model_root = std::env::var("DUB_STUDIO_HIGGS_MODEL")
            .map(PathBuf::from)
            .unwrap_or_else(|_| mroot.join("higgs-q8_0"));
        let fonts_dir = std::env::var("DUB_STUDIO_FONTS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| repo_root.join("fonts"));
        let voices_dir = std::env::var("DUBENGINE_VOICES")
            .map(PathBuf::from)
            .unwrap_or_else(|_| repo_root.join("voices"));
        AppState {
            repo_root,
            workspace,
            web_root,
            opts: Arc::new(EngineOpts::default()),
            jobs: JobQueue::new(),
            tdt_dir,
            sortformer_onnx,
            llama_bin,
            bsroformer_cli,
            bsroformer_model,
            higgs_dll,
            higgs_model_root,
            fonts_dir,
            models_root: mroot,
            voices_dir,
            setup_cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    fn proj_dir(&self, pid: &str) -> Result<PathBuf, Response> {
        // pid — hex uuid; отсекаем любые сепараторы/`..` до касания ФС (защита от traversal в pid).
        if pid.is_empty() || !pid.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err((StatusCode::NOT_FOUND, "project not found").into_response());
        }
        let d = self.workspace.join(pid);
        if !d.exists() {
            return Err((StatusCode::NOT_FOUND, "project not found").into_response());
        }
        Ok(d)
    }

    fn load_project(&self, pid: &str) -> Result<Project, Response> {
        let d = self.proj_dir(pid)?;
        let f = d.join("project.json");
        if !f.is_file() {
            return Err((StatusCode::CONFLICT, "project not analyzed yet").into_response());
        }
        let text = std::fs::read_to_string(&f)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response())?;
        Project::from_json(&text)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response())
    }
}

/// Атомарная запись project.json: сериализуем в tmp рядом, затем rename (tmp+rename — как в
/// проверенных питон-паттернах; частичного файла при падении не будет).
fn save_project_atomic(dir: &Path, proj: &Project) -> Result<(), String> {
    let json = proj
        .to_json_pretty()
        .map_err(|e| format!("сериализация project.json: {e}"))?;
    let tmp = dir.join("project.json.tmp");
    std::fs::write(&tmp, json.as_bytes()).map_err(|e| format!("запись tmp: {e}"))?;
    std::fs::rename(&tmp, dir.join("project.json")).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

pub fn build_router(state: AppState) -> Router {
    use tower_http::cors::{Any, CorsLayer};
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/engine/capabilities", get(capabilities))
        .route("/engine/opts", axum::routing::patch(endpoints::set_opts))
        .route("/engine/select", post(endpoints::select_model))
        // «Первый запуск»: статус компонентов + автозакачка недостающего (SSE через ту же job-машину).
        .route("/setup/status", get(setup_status))
        .route("/setup/download", post(setup_download))
        .route("/setup/cancel", post(setup_cancel))
        .route("/setup/browse", post(setup_browse))
        .route("/setup/import", post(setup_import))
        .route("/hw/snapshot", get(hw_snapshot))
        .route("/record/devices", get(record_devices))
        .route("/record/level", get(record_level))
        .route("/record/start", post(record_start))
        .route("/record/stop", post(record_stop))
        .route("/voices/download-pack", post(voices_download_pack))
        .route("/voices/catalog", get(voices_catalog))
        .route("/voices/sample", get(voice_sample))
        .route("/voices/get", post(voices_get))
        .route("/voices/rename", post(voices_rename))
        .route("/voices/delete", post(voices_delete))
        .route("/projects/{pid}/speaker-voice", post(speaker_voice))
        .route("/fonts", get(endpoints::fonts))
        .route("/voices", get(voices_list))
        .route("/presets", get(endpoints::presets))
        .route("/projects", post(create_project))
        .route(
            "/projects/{pid}",
            get(get_project).patch(patch_project).put(endpoints::put_project),
        )
        .route("/projects/{pid}/analyze", post(analyze_project))
        .route("/projects/{pid}/remix", post(endpoints::remix_project))
        .route("/projects/{pid}/render", post(render_project))
        .route("/projects/{pid}/waveform", get(endpoints::waveform))
        .route("/projects/{pid}/preview", get(endpoints::preview))
        .route("/projects/{pid}/output", get(output))
        .route("/projects/{pid}/open", post(open_output))
        .route("/projects/{pid}/reveal", post(reveal_file))
        .route("/projects/{pid}/save-text", post(save_text))
        .route("/projects/{pid}/dub-audio", post(dub_audio_project))
        // /original?t= отдаёт ОДИН PNG-кадр оригинала (порт app.py.original -> source_frame),
        // фронт (ComparePane) вставляет его как <img src>. Range-раздача сырого видео — /dub.
        .route("/projects/{pid}/original", get(endpoints::original_frame))
        .route("/projects/{pid}/dub", get(dub_video))
        .route("/jobs/{job_id}/events", get(job_events))
        // SPA fallback — монтируется последним, чтобы не затенять API.
        .fallback(spa_fallback)
        // Видео-аплоад — большие тела. axum по дефолту режет на 2МБ (multipart ломается на
        // реальном ролике). Питон (Starlette) лимита не ставит -> снимаем и мы.
        .layer(axum::extract::DefaultBodyLimit::disable())
        .layer(cors)
        .with_state(state)
}

// ─── /engine/capabilities ───────────────────────────────────────────────────

async fn capabilities(State(st): State<AppState>) -> Json<Value> {
    let o = &st.opts;
    let ffmpeg = which_ffmpeg();
    // Текущий выбор вариантов (active.json) — фронт рисует по нему селекторы движка/модели/кванта ASR.
    let sel = models::load_selection(&st.models_root);
    // Языки контента = полный набор Whisper (99). Единый источник — dub_translate::WHISPER_LANGS.
    let langs: Vec<&str> = dub_translate::WHISPER_LANGS.iter().map(|(c, _)| *c).collect();
    // Тот же JSON-контракт, что в app.py.capabilities(), плюс поля выбора ASR-движка.
    Json(json!({
        "device": o.device,
        "tts_quant": o.tts_quant,
        "asr_model": o.asr_model,
        "models": {
            "asr": o.asr_model,
            "llm": o.mt_model_path.to_string_lossy(),
            "vision": o.mmproj_path.to_string_lossy(),
            "tts": o.tts_model,
        },
        "ffmpeg": ffmpeg,
        "languages": langs,
        "voice_modes": ["clone","autocast","auto","voice"],
        // Выбор ASR: движок (parakeet|whisper), модель Whisper, квант Whisper (compute_type).
        "selection": sel,
        "asr_engines": ["parakeet","whisper"],
        "whisper_models": ["tiny","base","small","medium","large-v3","large-v3-turbo"],
        // Только CPU-безопасные кванты: float16/bfloat16 GPU-only и роняют CTranslate2 на CPU (onefile
        // бинарь идёт без CUDA-либ). int8 — дефолт (быстро), float32 — максимум точности.
        "whisper_computes": ["int8","int8_float32","float32"],
        // Видимые лимиты RAM (настройки, не авто-магия): против OOM на слабой памяти. "0" = авто (дефолт).
        "llama_ubatches": ["0","512","256","128"],      // prefill-батч Gemma (меньше = меньше RAM)
        "higgs_ref_secs_opts": ["12","8","6","4"],       // длина реф-клипа клона (сек; <12 спасает 32ГБ)
    }))
}

fn which_ffmpeg() -> bool {
    let name = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| dir.join(name).is_file())
        })
        .unwrap_or(false)
}

// ─── /setup/status ; /setup/download ; /setup/cancel ────────────────────────

/// GET /setup/status — статус каждого компонента (installed/missing/размер) + драйвер/готовность.
/// Читает только диск (никаких моделей не грузит) — безопасно вызывать до любой закачки.
async fn setup_status(State(st): State<AppState>) -> Json<Value> {
    let root = st.repo_root.clone();
    // ФС-обход в блокирующий пул (десятки stat-ов), чтобы не держать реактор.
    let status = tokio::task::spawn_blocking(move || setup::setup_status(&root))
        .await
        .unwrap_or_else(|_| setup::setup_status(&st.repo_root));
    Json(serde_json::to_value(status).unwrap_or_else(|_| json!({})))
}

/// POST /setup/download {ids:[...]} — поставить джобу закачки выбранных компонентов; вернуть job_id.
/// Прогресс — по SSE GET /jobs/{id}/events (та же машина, что analyze/render).
async fn setup_download(State(st): State<AppState>, Json(body): Json<Value>) -> Response {
    let ids: Vec<String> = body
        .get("ids")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();
    if ids.is_empty() {
        return (StatusCode::BAD_REQUEST, "ids пуст").into_response();
    }
    let root = st.repo_root.clone();
    let cancel_flag = st.setup_cancel.clone();
    cancel_flag.store(false, std::sync::atomic::Ordering::SeqCst); // свежий старт

    let job: jobs::JobFn = Box::new(move |progress: jobs::ProgressFn| {
        let cancel = {
            let cf = cancel_flag.clone();
            move || cf.load(std::sync::atomic::Ordering::SeqCst)
        };
        let cb = |ev: Value| progress(ev);
        setup::download_components(&root, &ids, &cancel, &cb)
    });
    let job_id = st.jobs.enqueue(job).await;
    Json(json!({ "job_id": job_id })).into_response()
}

/// POST /setup/cancel — взвести флаг отмены текущей закачки (download-loop прервётся на следующем чанке).
async fn setup_cancel(State(st): State<AppState>) -> Json<Value> {
    st.setup_cancel.store(true, std::sync::atomic::Ordering::SeqCst);
    Json(json!({ "cancelled": true }))
}

/// GET /hw/snapshot — снимок GPU/VRAM/темп/мощность + RAM для монитора ресурсов.
async fn hw_snapshot() -> Json<hw::HardwareSnapshot> {
    Json(tokio::task::spawn_blocking(hw::snapshot).await.unwrap_or_default())
}

/// Безопасное имя голоса: только буквы/цифры/пробел/дефис/подчёркивание (без разделителей путей).
fn sanitize_voice_name(s: &str) -> String {
    let n: String = s
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-' || *c == '_')
        .take(48)
        .collect();
    let n = n.trim().to_string();
    if n.is_empty() { "Мой голос".to_string() } else { n }
}

/// Имена голосов в каталоге: стемы .wav/.mp3 (запись с микрофона = .wav; пак = .mp3).
fn list_voice_names(dir: &Path) -> Vec<String> {
    let mut names = std::collections::BTreeSet::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("").to_lowercase();
            if ext == "wav" || ext == "mp3" {
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    names.insert(stem.to_string());
                }
            }
        }
    }
    names.into_iter().collect()
}

/// GET /voices — список голосов из каталога (пак + записи с микрофона).
async fn voices_list(State(st): State<AppState>) -> Json<Value> {
    Json(json!({ "voices": list_voice_names(&st.voices_dir) }))
}

/// GET /voices/sample?name=<name> — отдать аудио-сэмпл голоса (voices/<name>.wav|.mp3) с Range для <audio>
/// (прослушка выбранного голоса в UI). sanitize защищает от path-traversal.
async fn voice_sample(
    State(st): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
    req: axum::http::Request<axum::body::Body>,
) -> Response {
    let name = sanitize_voice_name(q.get("name").map(|s| s.as_str()).unwrap_or(""));
    if name.is_empty() {
        return (StatusCode::BAD_REQUEST, "no name").into_response();
    }
    for ext in ["wav", "mp3"] {
        let p = st.voices_dir.join(format!("{name}.{ext}"));
        if p.is_file() {
            return serve_file_range(&p, req, None).await;
        }
    }
    (StatusCode::NOT_FOUND, "voice not found").into_response()
}

/// GET /record/devices — список микрофонов.
async fn record_devices() -> Json<Value> {
    Json(json!({ "devices": tokio::task::spawn_blocking(record::input_devices).await.unwrap_or_default() }))
}

/// GET /record/level — текущий пик 0..1 (метр уровня).
async fn record_level() -> Json<Value> {
    Json(json!({ "level": record::level() }))
}

/// POST /record/start {name, device?} — начать запись голоса в voices/<name>.wav.
async fn record_start(State(st): State<AppState>, Json(body): Json<Value>) -> Json<Value> {
    let name = sanitize_voice_name(body.get("name").and_then(|v| v.as_str()).unwrap_or("Мой голос"));
    let device = body.get("device").and_then(|v| v.as_str()).map(|s| s.to_string());
    let path = st.voices_dir.join(format!("{name}.wav"));
    match tokio::task::spawn_blocking(move || record::start(&path, device)).await {
        Ok(Ok(())) => Json(json!({ "ok": true, "name": name })),
        Ok(Err(e)) => Json(json!({ "ok": false, "error": e })),
        Err(e) => Json(json!({ "ok": false, "error": e.to_string() })),
    }
}

/// POST /record/stop — остановить запись; вернуть имя записанного голоса + обновлённый список.
async fn record_stop(State(st): State<AppState>) -> Json<Value> {
    let path = tokio::task::spawn_blocking(record::stop).await;
    let name = match path {
        Ok(Ok(p)) => p.file_stem().and_then(|s| s.to_str()).map(|s| s.to_string()),
        _ => None,
    };
    Json(json!({ "name": name, "voices": list_voice_names(&st.voices_dir) }))
}

/// POST /projects/{pid}/speaker-voice {speaker, name} — сделать голос из спикера: вырезать его
/// длиннейшую реплику из источника → voices/<name>.wav (16k mono, <=12с). Порт «Сделать голос» Higgs.
async fn speaker_voice(
    State(st): State<AppState>,
    axum::extract::Path(pid): axum::extract::Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let proj = match st.load_project(&pid) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let dir = match st.proj_dir(&pid) {
        Ok(d) => d,
        Err(r) => return r,
    };
    let want = body.get("speaker").and_then(|v| v.as_str()).unwrap_or("0").to_string();
    let name = sanitize_voice_name(body.get("name").and_then(|v| v.as_str()).unwrap_or(""));
    // длиннейшая реплика спикера.
    let cand = proj
        .segments
        .iter()
        .filter(|s| s.speaker.clone().unwrap_or_else(|| "0".into()) == want)
        .max_by(|a, b| (a.end - a.start).partial_cmp(&(b.end - b.start)).unwrap_or(std::cmp::Ordering::Equal));
    let Some(cand) = cand else {
        return (StatusCode::BAD_REQUEST, "у спикера нет реплик").into_response();
    };
    let (start, end) = (cand.start, cand.end.min(cand.start + 12.0));
    let ref_text = cand.src_text.trim().to_string();   // реф-текст = расшифровка реплики (как Higgs build_speaker_reference)
    let input = std::fs::read_to_string(dir.join("source.txt")).unwrap_or_default();
    let input = if !input.trim().is_empty() { PathBuf::from(input.trim()) } else { dir.join("source.mp4") };
    let out = st.voices_dir.join(format!("{name}.wav"));
    let txt = st.voices_dir.join(format!("{name}.txt"));
    let voices_dir = st.voices_dir.clone();
    let cli = st.bsroformer_cli.clone();
    let model = st.bsroformer_model.clone();
    let tmp = dir.join("_voicecut");
    let res = tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&voices_dir).ok();
        std::fs::create_dir_all(&tmp).ok();
        let raw = tmp.join("cut.wav");
        media::trim(&input, &raw, start, end, 16_000)?;
        // очистка голоса: вокал-сепарация (Mel-Band Roformer) убирает фон/музыку из рефа (как voiceclean в
        // Higgs) — клон цепляется за чистый голос. Движку нужен 44.1к; без установленного движка -> сырой клип.
        let src_for_ref = if cli.is_file() && model.is_file() {
            let clip44 = tmp.join("cut44.wav");
            match media::extract_audio(&raw, &clip44, 44_100, 2)
                .and_then(|_| dub_sep::separate(&clip44, &tmp.join("stems"), &cli, &model).map_err(|e| e.to_string()))
            {
                Ok(sep) => sep.vocals,
                Err(_) => raw.clone(),
            }
        } else {
            raw.clone()
        };
        media::to_16k_mono(&src_for_ref, &out)?;   // реф в 16k mono
        if !ref_text.is_empty() {
            let _ = std::fs::write(&txt, &ref_text);   // голос несёт свой ref-текст в библиотеке
        }
        let _ = std::fs::remove_dir_all(&tmp);
        Ok::<(), String>(())
    })
    .await;
    match res {
        Ok(Ok(())) => Json(json!({ "ok": true, "name": name, "voices": list_voice_names(&st.voices_dir) })).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// GET /voices/catalog — каталог доп-голосов (HF датасет Slait/russia_voices): имя, пол, URL-превью.
async fn voices_catalog() -> Json<Value> {
    Json(tokio::task::spawn_blocking(record::catalog).await.unwrap_or_else(|_| json!({ "voices": [] })))
}

/// POST /voices/get {name} — скачать один голос (mp3+txt) из датасета в каталог голосов.
async fn voices_get(State(st): State<AppState>, Json(body): Json<Value>) -> Json<Value> {
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let dir = st.voices_dir.clone();
    let ok = tokio::task::spawn_blocking(move || record::fetch_voice(&dir, &name)).await;
    match ok {
        Ok(Ok(())) => Json(json!({ "ok": true, "voices": list_voice_names(&st.voices_dir) })),
        Ok(Err(e)) => Json(json!({ "ok": false, "error": e })),
        Err(e) => Json(json!({ "ok": false, "error": e.to_string() })),
    }
}

/// POST /voices/rename {from, to} — переименовать голос (mp3/wav + txt).
async fn voices_rename(State(st): State<AppState>, Json(body): Json<Value>) -> Json<Value> {
    let from = sanitize_voice_name(body.get("from").and_then(|v| v.as_str()).unwrap_or(""));
    let to = sanitize_voice_name(body.get("to").and_then(|v| v.as_str()).unwrap_or(""));
    for ext in ["wav", "mp3", "txt"] {
        let src = st.voices_dir.join(format!("{from}.{ext}"));
        if src.is_file() {
            let _ = std::fs::rename(&src, st.voices_dir.join(format!("{to}.{ext}")));
        }
    }
    Json(json!({ "voices": list_voice_names(&st.voices_dir) }))
}

/// POST /voices/delete {name} — удалить голос.
async fn voices_delete(State(st): State<AppState>, Json(body): Json<Value>) -> Json<Value> {
    let name = sanitize_voice_name(body.get("name").and_then(|v| v.as_str()).unwrap_or(""));
    for ext in ["wav", "mp3", "txt"] {
        let _ = std::fs::remove_file(st.voices_dir.join(format!("{name}.{ext}")));
    }
    Json(json!({ "voices": list_voice_names(&st.voices_dir) }))
}

/// POST /voices/download-pack — скачать пак голосов (VibeVoice) и распаковать в каталог голосов.
async fn voices_download_pack(State(st): State<AppState>) -> Response {
    let dir = st.voices_dir.clone();
    let job: jobs::JobFn = Box::new(move |progress: jobs::ProgressFn| {
        let cb = |ev: Value| progress(ev);
        record::download_pack(&dir, &cb)
    });
    let job_id = st.jobs.enqueue(job).await;
    Json(json!({ "job_id": job_id })).into_response()
}

/// POST /setup/browse — открыть нативный диалог выбора папки, импортировать оттуда готовые модели по
/// маркерам (без докачки). {picked:bool, imported:[...], status}.
async fn setup_browse(State(st): State<AppState>, Json(body): Json<Value>) -> Json<Value> {
    let root = st.repo_root.clone();
    let only = body.get("id").and_then(|v| v.as_str()).map(|s| s.to_string());
    let res = tokio::task::spawn_blocking(move || {
        let title = if only.is_some() { "Файл(ы) модели" } else { "Папка с готовыми моделями" };
        let dir = rfd::FileDialog::new().set_title(title).pick_folder();
        match dir {
            Some(d) => (true, setup::import_from_dir(&root, &d, only.as_deref())),
            None => (false, Vec::new()),
        }
    })
    .await
    .unwrap_or((false, Vec::new()));
    Json(json!({ "picked": res.0, "imported": res.1, "status": setup::setup_status(&st.repo_root) }))
}

/// POST /setup/import {path, id?} — импортировать готовые модели из заданной папки (без диалога).
async fn setup_import(State(st): State<AppState>, Json(body): Json<Value>) -> Json<Value> {
    let path = body.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let only = body.get("id").and_then(|v| v.as_str());
    let imported = if path.is_empty() {
        Vec::new()
    } else {
        setup::import_from_dir(&st.repo_root, std::path::Path::new(path), only)
    };
    Json(json!({ "imported": imported, "status": setup::setup_status(&st.repo_root) }))
}

// ─── POST /projects (multipart video upload) ────────────────────────────────

async fn create_project(
    State(st): State<AppState>,
    mut multipart: Multipart,
) -> Response {
    let pid = uuid::Uuid::new_v4().simple().to_string();
    let pid = pid[..12].to_string();
    let d = st.workspace.join(&pid);
    if let Err(e) = tokio::fs::create_dir_all(&d).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }

    let mut filename: Option<String> = None;
    while let Ok(Some(field)) = multipart.next_field().await {
        // Поле файла (в app.py параметр называется `file`); берём первое с именем файла.
        let fname = field.file_name().map(|s| s.to_string());
        let data = match field.bytes().await {
            Ok(b) => b,
            Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
        };
        let ext = fname
            .as_deref()
            .and_then(|f| Path::new(f).extension())
            .and_then(|e| e.to_str())
            .unwrap_or("mp4");
        let dst = d.join(format!("source.{ext}"));
        if let Err(e) = tokio::fs::write(&dst, &data).await {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
        // source.txt хранит абсолютный путь к видео (как в app.py).
        let _ = tokio::fs::write(d.join("source.txt"), dst.to_string_lossy().as_bytes()).await;
        filename = fname;
        break;
    }

    Json(json!({ "project_id": pid, "filename": filename })).into_response()
}

// ─── GET /projects/{pid} ────────────────────────────────────────────────────

async fn get_project(State(st): State<AppState>, AxPath(pid): AxPath<String>) -> Response {
    match st.load_project(&pid) {
        Ok(p) => match p.to_json() {
            Ok(s) => ([("content-type", "application/json")], s).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        },
        Err(resp) => resp,
    }
}

// ─── POST /projects/{pid}/analyze ───────────────────────────────────────────

async fn analyze_project(
    State(st): State<AppState>,
    AxPath(pid): AxPath<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let dir = match st.proj_dir(&pid) {
        Ok(d) => d,
        Err(resp) => return resp,
    };
    // Исходный путь видео из source.txt (как в app.py). Fallback — source.* в каталоге.
    let src_txt = dir.join("source.txt");
    let input = match std::fs::read_to_string(&src_txt) {
        Ok(s) => PathBuf::from(s.trim()),
        Err(_) => {
            return (StatusCode::CONFLICT, "no source uploaded").into_response();
        }
    };
    if !input.is_file() {
        return (StatusCode::CONFLICT, "source video missing").into_response();
    }

    // Параметры analyze из query (дефолты как в app.py.analyze_project).
    let qget = |k: &str, d: &str| q.get(k).cloned().unwrap_or_else(|| d.to_string());
    let args = analyze::AnalyzeArgs {
        tgt_lang: qget("tgt_lang", "en"),
        mode: qget("mode", "auto"),
        src_lang: qget("src_lang", "auto"),
        subs: qget("subs", "auto"),
        rewrite: qget("rewrite", ""),
        burn: qget("burn", "1") != "0",
        detect_text: qget("detect", "1") != "0",
    };
    // Активный вариант модели резолвится ПРИ КАЖДОЙ джобе (не морозится на старте): скачал/выбрал
    // квант -> применяется без рестарта. См. models::resolve_*.
    let sel = models::load_selection(&st.models_root);
    let (mt_model, mmproj) = models::resolve_mt(&st.models_root, &sel);
    let asr = models::resolve_asr_choice(&st.repo_root, &st.models_root, &sel);
    eprintln!("[models] analyze: MT={} · ASR={}", mt_model.display(), asr.describe());
    let paths = analyze::AnalyzePaths {
        input,
        work_dir: dir.clone(),
        asr,
        sortformer_onnx: st.sortformer_onnx.clone(),
        llama_bin: st.llama_bin.clone(),
        mt_model,
        mmproj,
        models_root: st.models_root.clone(),
        caption_fps: st.opts.caption_fps,
    };

    // Тело джобы: analyze -> project.json (атомарно). Прогресс -> SSE.
    let dir_for_save = dir.clone();
    let pid_for_result = pid.clone();
    let job: jobs::JobFn = Box::new(move |progress: jobs::ProgressFn| {
        let cb = |ev: Value| progress(ev);
        let proj = analyze::run(&args, &paths, &cb)?;
        save_project_atomic(&dir_for_save, &proj)?;
        Ok(json!({ "project_id": pid_for_result, "output": dir_for_save.join("project.json").to_string_lossy() }))
    });
    let job_id = st.jobs.enqueue(job).await;
    Json(json!({ "job_id": job_id })).into_response()
}

// ─── PATCH /projects/{pid} ──────────────────────────────────────────────────

async fn patch_project(
    State(st): State<AppState>,
    AxPath(pid): AxPath<String>,
    Json(edit): Json<Value>,
) -> Response {
    let dir = match st.proj_dir(&pid) {
        Ok(d) => d,
        Err(resp) => return resp,
    };
    let mut proj = match st.load_project(&pid) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    if let Err((code, msg)) = patch::apply(&mut proj, &edit) {
        let status = StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_REQUEST);
        return (status, msg).into_response();
    }
    if let Err(e) = save_project_atomic(&dir, &proj) {
        return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
    }
    match proj.to_json() {
        Ok(s) => ([("content-type", "application/json")], s).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// ─── POST /projects/{pid}/render ────────────────────────────────────────────

async fn render_project(State(st): State<AppState>, AxPath(pid): AxPath<String>) -> Response {
    let dir = match st.proj_dir(&pid) {
        Ok(d) => d,
        Err(resp) => return resp,
    };
    let proj_path = dir.join("project.json");
    if !proj_path.is_file() {
        return (StatusCode::CONFLICT, "project not analyzed yet").into_response();
    }
    // Исходный путь видео из source.txt (как в analyze/app.py).
    let input = match std::fs::read_to_string(dir.join("source.txt")) {
        Ok(s) => PathBuf::from(s.trim()),
        Err(_) => return (StatusCode::CONFLICT, "no source uploaded").into_response(),
    };
    let output = dir.join("output.mp4");

    // Активный вариант модели резолвится ПРИ КАЖДОЙ джобе (см. models::resolve_*): скачал/выбрал
    // квант в настройках -> применяется без рестарта сервера.
    let sel = models::load_selection(&st.models_root);
    let (higgs_model_root, higgs_quant) = models::resolve_tts(&st.models_root, &sel);
    let sep_model = models::resolve_sep(&st.models_root, &sel);
    eprintln!("[models] render: TTS={} (q={}) · SEP={}", higgs_model_root.display(), higgs_quant, sep_model.display());
    let paths = render::RenderPaths {
        input,
        work_dir: dir.clone(),
        output: output.clone(),
        bsroformer_cli: st.bsroformer_cli.clone(),
        bsroformer_model: sep_model,
        higgs_dll: st.higgs_dll.clone(),
        higgs_model_root,
        higgs_quant,
        fonts_dir: st.fonts_dir.clone(),
        higgs_backend: st.opts.device.clone(),
        higgs_device: 0,
        higgs_threads: st.opts.num_threads,
        max_stretch: st.opts.max_stretch as f64,
        voices_dir: st.voices_dir.clone(),
        asr: models::resolve_asr_choice(&st.repo_root, &st.models_root, &sel),
        ref_secs: models::higgs_ref_secs(&st.models_root),
    };

    let dir_for_job = dir.clone();
    let out_for_result = output.clone();
    let job: jobs::JobFn = Box::new(move |progress: jobs::ProgressFn| {
        let cb = |ev: Value| progress(ev);
        // Загрузить свежий Project (правки могли прийти после enqueue).
        let text = std::fs::read_to_string(&proj_path).map_err(|e| e.to_string())?;
        let proj = Project::from_json(&text).map_err(|e| e.to_string())?;
        // regen_dub если есть dirty-сегменты (voice/text/rewrite правились).
        let regen = proj.segments.iter().any(|s| s.dirty);
        render::run(&proj, &paths, regen, &cb)?;
        // Правки запечены в дубляж -> сбросить dirty (перечитать, чтобы не затереть правки во время рендера).
        if regen {
            let baked: std::collections::HashMap<String, String> =
                proj.segments.iter().map(|s| (s.id.clone(), s.tgt_text.clone())).collect();
            if let Ok(t2) = std::fs::read_to_string(&proj_path) {
                if let Ok(mut cur) = Project::from_json(&t2) {
                    for s in &mut cur.segments {
                        if baked.get(&s.id) == Some(&s.tgt_text) {
                            s.dirty = false;
                        }
                    }
                    let _ = save_project_atomic(&dir_for_job, &cur);
                }
            }
        }
        Ok(json!({ "output": out_for_result.to_string_lossy() }))
    });
    let job_id = st.jobs.enqueue(job).await;
    Json(json!({ "job_id": job_id })).into_response()
}

/// POST /projects/{pid}/dub-audio — сгенерить ТОЛЬКО озвучку (без сборки видео) -> dub_audio.m4a.
/// Фронт вызывает сразу после анализа, чтобы дуб можно было слушать в редакторе (плей играет /dub),
/// не дожидаясь и не собирая финальное видео. Тот же job-паттерн, что и render_project.
async fn dub_audio_project(State(st): State<AppState>, AxPath(pid): AxPath<String>) -> Response {
    let dir = match st.proj_dir(&pid) {
        Ok(d) => d,
        Err(resp) => return resp,
    };
    let proj_path = dir.join("project.json");
    if !proj_path.is_file() {
        return (StatusCode::CONFLICT, "project not analyzed yet").into_response();
    }
    let input = match std::fs::read_to_string(dir.join("source.txt")) {
        Ok(s) => PathBuf::from(s.trim()),
        Err(_) => return (StatusCode::CONFLICT, "no source uploaded").into_response(),
    };
    let sel = models::load_selection(&st.models_root);
    let (higgs_model_root, higgs_quant) = models::resolve_tts(&st.models_root, &sel);
    let paths = render::RenderPaths {
        input,
        work_dir: dir.clone(),
        output: dir.join("output.mp4"),
        bsroformer_cli: st.bsroformer_cli.clone(),
        bsroformer_model: models::resolve_sep(&st.models_root, &sel),
        higgs_dll: st.higgs_dll.clone(),
        higgs_model_root,
        higgs_quant,
        fonts_dir: st.fonts_dir.clone(),
        higgs_backend: st.opts.device.clone(),
        higgs_device: 0,
        higgs_threads: st.opts.num_threads,
        max_stretch: st.opts.max_stretch as f64,
        voices_dir: st.voices_dir.clone(),
        asr: models::resolve_asr_choice(&st.repo_root, &st.models_root, &sel),
        ref_secs: models::higgs_ref_secs(&st.models_root),
    };
    let job: jobs::JobFn = Box::new(move |progress: jobs::ProgressFn| {
        let cb = |ev: Value| progress(ev);
        let text = std::fs::read_to_string(&proj_path).map_err(|e| e.to_string())?;
        let proj = Project::from_json(&text).map_err(|e| e.to_string())?;
        let regen = proj.segments.iter().any(|s| s.dirty);
        let out = render::dub_audio(&proj, &paths, regen, &cb)?;
        Ok(json!({ "audio": out.to_string_lossy() }))
    });
    let job_id = st.jobs.enqueue(job).await;
    Json(json!({ "job_id": job_id })).into_response()
}

// ─── GET /projects/{pid}/output ; /original ; /dub (Range-раздача файла) ─────

async fn output(
    State(st): State<AppState>,
    AxPath(pid): AxPath<String>,
    Query(q): Query<HashMap<String, String>>,
    req: axum::http::Request<axum::body::Body>,
) -> Response {
    let dir = match st.proj_dir(&pid) {
        Ok(d) => d,
        Err(resp) => return resp,
    };
    let f = dir.join("output.mp4");
    if !f.is_file() {
        return (StatusCode::NOT_FOUND, "not rendered").into_response();
    }
    let dl = q.get("dl").map(|v| v == "1").unwrap_or(false);
    let filename = if dl { Some(format!("{pid}_dub.mp4")) } else { None };
    serve_file_range(&f, req, filename).await
}

/// POST /projects/{pid}/open — открыть готовый output.mp4 в СИСТЕМНОМ плеере (на машине пользователя).
/// Нужно, т.к. в нативном Tauri-webview <a target="_blank"> внешний плеер не открывает.
async fn open_output(State(st): State<AppState>, AxPath(pid): AxPath<String>) -> Response {
    let dir = match st.proj_dir(&pid) {
        Ok(d) => d,
        Err(r) => return r,
    };
    let f = dir.join("output.mp4");
    if !f.is_file() {
        return (StatusCode::NOT_FOUND, "not rendered").into_response();
    }
    let path = f.to_string_lossy().to_string();
    let _ = tokio::task::spawn_blocking(move || {
        #[cfg(windows)]
        {
            let _ = std::process::Command::new("cmd").args(["/C", "start", "", &path]).spawn();
        }
        #[cfg(not(windows))]
        {
            let _ = std::process::Command::new("xdg-open").arg(&path).spawn();
        }
    })
    .await;
    Json(json!({ "ok": true })).into_response()
}

/// Открыть проводник/файловый менеджер с ВЫДЕЛЕННЫМ файлом (не плеер). Windows: explorer /select.
fn reveal_in_explorer(path: String) {
    tokio::task::spawn_blocking(move || {
        #[cfg(windows)]
        {
            // explorer /select,"<path>" — выделяет файл в открытом каталоге. Один аргумент.
            let _ = std::process::Command::new("explorer").arg(format!("/select,{path}")).spawn();
        }
        #[cfg(not(windows))]
        {
            // прочие ОС: открыть родительский каталог (выделение файла непортабельно).
            let parent = std::path::Path::new(&path).parent().map(|p| p.to_path_buf()).unwrap_or_else(|| std::path::PathBuf::from("."));
            let _ = std::process::Command::new("xdg-open").arg(parent).spawn();
        }
    });
}

/// POST /projects/{pid}/reveal — показать файл (по имени в каталоге проекта) в проводнике с выделением.
/// Body {name}. Для кнопок сохранения/экспорта: пользователь видит, КУДА сохранилось.
async fn reveal_file(State(st): State<AppState>, AxPath(pid): AxPath<String>, Json(body): Json<Value>) -> Response {
    let dir = match st.proj_dir(&pid) {
        Ok(d) => d,
        Err(r) => return r,
    };
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("output.mp4");
    let safe: String = name.chars().filter(|c| c.is_alphanumeric() || matches!(c, '.' | '_' | '-')).collect();
    let f = dir.join(&safe);
    if !f.is_file() {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    reveal_in_explorer(f.to_string_lossy().to_string());
    Json(json!({ "ok": true })).into_response()
}

/// POST /projects/{pid}/save-text — записать текстовый файл (SRT/TXT) в каталог проекта и показать его в
/// проводнике. В нативном Tauri-webview браузерный blob-download (<a download>) не работает — сохраняем
/// через бэкенд. Body {name, text}.
async fn save_text(State(st): State<AppState>, AxPath(pid): AxPath<String>, Json(body): Json<Value>) -> Response {
    let dir = match st.proj_dir(&pid) {
        Ok(d) => d,
        Err(r) => return r,
    };
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("transcript.txt");
    let safe: String = name.chars().filter(|c| c.is_alphanumeric() || matches!(c, '.' | '_' | '-')).collect();
    let safe = if safe.is_empty() { "transcript.txt".to_string() } else { safe };
    let text = body.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let f = dir.join(&safe);
    if let Err(e) = std::fs::write(&f, text) {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("write failed: {e}")).into_response();
    }
    reveal_in_explorer(f.to_string_lossy().to_string());
    Json(json!({ "ok": true, "path": f.to_string_lossy() })).into_response()
}

async fn dub_video(
    State(st): State<AppState>,
    AxPath(pid): AxPath<String>,
    req: axum::http::Request<axum::body::Body>,
) -> Response {
    let dir = match st.proj_dir(&pid) {
        Ok(d) => d,
        Err(resp) => return resp,
    };
    // Что играет <audio>: собранный output.mp4 (после рендера), иначе dub_audio.m4a (озвучка после
    // анализа — слушать дуб, не собирая видео), иначе analyzed.mp4 (оригинальная дорожка).
    let mut f = dir.join("output.mp4");
    if !f.is_file() {
        f = dir.join("dub_audio.m4a");
    }
    if !f.is_file() {
        f = dir.join("analyzed.mp4");
    }
    if !f.is_file() {
        // nodub / субтитры / транскрипт: озвучки нет — играем ОРИГИНАЛЬНУЮ дорожку видео (source.*),
        // чтобы плей в редакторе работал (переиспользуем аудиодорожку исходника, <audio> берёт её из mp4).
        // Путь — из source.txt (как в analyze/render); fallback — source.* в каталоге.
        if let Ok(p) = std::fs::read_to_string(dir.join("source.txt")) {
            let sp = std::path::PathBuf::from(p.trim());
            if sp.is_file() {
                f = sp;
            }
        }
        if !f.is_file() {
            if let Ok(rd) = std::fs::read_dir(&dir) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.file_stem().and_then(|s| s.to_str()) == Some("source") && p.is_file() {
                        f = p;
                        break;
                    }
                }
            }
        }
    }
    if !f.is_file() {
        return (StatusCode::NOT_FOUND, "no dubbed audio yet").into_response();
    }
    serve_file_range(&f, req, None).await
}

/// Отдать файл с поддержкой Range (для <video> seek). Используем tower-http ServeFile — он
/// корректно обрабатывает Range/If-Range/Content-Range. dl -> Content-Disposition attachment.
async fn serve_file_range(
    path: &Path,
    req: axum::http::Request<axum::body::Body>,
    download_name: Option<String>,
) -> Response {
    use tower::ServiceExt;
    use tower_http::services::ServeFile;
    let svc = ServeFile::new(path);
    match svc.oneshot(req).await {
        Ok(mut resp) => {
            if let Some(name) = download_name {
                if let Ok(v) = axum::http::HeaderValue::from_str(&format!(
                    "attachment; filename=\"{name}\""
                )) {
                    resp.headers_mut().insert(axum::http::header::CONTENT_DISPOSITION, v);
                }
            }
            resp.map(axum::body::Body::new)
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// ─── GET /jobs/{job_id}/events (SSE) ────────────────────────────────────────

async fn job_events(
    State(st): State<AppState>,
    AxPath(job_id): AxPath<String>,
) -> Response {
    let Some((rx, terminal)) = st.jobs.subscribe(&job_id).await else {
        return (StatusCode::NOT_FOUND, "job not found").into_response();
    };
    let jobs = st.jobs.clone();
    let stream = sse_stream(rx, terminal, jobs, job_id);
    Sse::new(stream).into_response()
}

/// Поток SSE-событий джобы. Завершается на событии done/error и реапит терминальную джобу.
fn sse_stream(
    mut rx: tokio::sync::broadcast::Receiver<Value>,
    terminal: Option<Value>,
    jobs: JobQueue,
    job_id: String,
) -> impl Stream<Item = Result<Event, Infallible>> {
    async_stream::stream! {
        // Джоба уже завершена к моменту подписки — сразу отдать терминал и выйти.
        if let Some(ev) = terminal {
            yield sse_event(&ev);
            if jobs.is_terminal(&job_id).await {
                jobs.remove(&job_id).await;
            }
            return;
        }
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let is_terminal = matches!(
                        ev.get("type").and_then(|t| t.as_str()),
                        Some("done") | Some("error")
                    );
                    yield sse_event(&ev);
                    if is_terminal {
                        if jobs.is_terminal(&job_id).await {
                            jobs.remove(&job_id).await;
                        }
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break, // канал закрыт
            }
        }
    }
}

fn sse_event(ev: &Value) -> Result<Event, Infallible> {
    Ok(Event::default().data(serde_json::to_string(ev).unwrap_or_default()))
}

// ─── SPA fallback ───────────────────────────────────────────────────────────

async fn spa_fallback(State(st): State<AppState>, uri: axum::http::Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    spa::serve_spa(st.web_root.as_deref(), path).await
}
