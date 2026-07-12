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
mod jobs;
mod media;
mod ocr;
mod patch;
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
/// затем вжигаем блюр+сабы в captioned.mp4. Так проверяем items 1-5/8 аудита на живых данных, не
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
    // прошлый прогон, чтобы видеть свежий результат (не смешивать со старым blur из project.json).
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
    };
    let paths = analyze::AnalyzePaths {
        input: input_video.to_path_buf(),
        work_dir: work_dir.to_path_buf(),
        tdt_dir: unused.clone(),
        sortformer_onnx: unused.clone(),
        llama_bin: dub_llm::resolve_llama_bin(&repo_root.join("tools").join("llama")),
        mt_model: opts.mt_model_path.clone(),
        mmproj: opts.mmproj_path.clone(),
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
    /// Флаг отмены текущей setup-закачки (POST /setup/cancel взводит; download-loop его читает).
    pub setup_cancel: Arc<std::sync::atomic::AtomicBool>,
}

/// Корень моделей: env DUBENGINE_MODELS_ROOT, иначе <repo_root>/models.
fn models_root(repo_root: &Path) -> PathBuf {
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
        // «Первый запуск»: статус компонентов + автозакачка недостающего (SSE через ту же job-машину).
        .route("/setup/status", get(setup_status))
        .route("/setup/download", post(setup_download))
        .route("/setup/cancel", post(setup_cancel))
        .route("/fonts", get(endpoints::fonts))
        .route("/voices", get(endpoints::voices))
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
    // Тот же JSON-контракт, что в app.py.capabilities().
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
        "languages": ["en","ru","zh","es","pt","fr"],
        "voice_modes": ["clone","autocast","auto","voice"],
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
    };
    let paths = analyze::AnalyzePaths {
        input,
        work_dir: dir.clone(),
        tdt_dir: st.tdt_dir.clone(),
        sortformer_onnx: st.sortformer_onnx.clone(),
        llama_bin: st.llama_bin.clone(),
        mt_model: st.opts.mt_model_path.clone(),
        mmproj: st.opts.mmproj_path.clone(),
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

    let paths = render::RenderPaths {
        input,
        work_dir: dir.clone(),
        output: output.clone(),
        bsroformer_cli: st.bsroformer_cli.clone(),
        bsroformer_model: st.bsroformer_model.clone(),
        higgs_dll: st.higgs_dll.clone(),
        higgs_model_root: st.higgs_model_root.clone(),
        fonts_dir: st.fonts_dir.clone(),
        higgs_backend: st.opts.device.clone(),
        higgs_device: 0,
        higgs_threads: st.opts.num_threads,
        max_stretch: st.opts.max_stretch as f64,
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

async fn dub_video(
    State(st): State<AppState>,
    AxPath(pid): AxPath<String>,
    req: axum::http::Request<axum::body::Body>,
) -> Response {
    let dir = match st.proj_dir(&pid) {
        Ok(d) => d,
        Err(resp) => return resp,
    };
    // Проигрываемое видео: output.mp4, иначе analyzed.mp4.
    let mut f = dir.join("output.mp4");
    if !f.is_file() {
        f = dir.join("analyzed.mp4");
    }
    if !f.is_file() {
        return (StatusCode::NOT_FOUND, "no dubbed video yet").into_response();
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
