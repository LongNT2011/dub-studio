//! dub-server — axum-реализация REST/SSE контракта Dub Studio. Цель: SPA (frontend/dist) работает
//! без правок против этого сервера так же, как против backend/app.py.
//!
//! Раунд 1 реализует: раздачу SPA с защитой от traversal, GET /engine/capabilities, POST /projects
//! (multipart-загрузка видео -> workspace/<pid>/), GET /projects/{id}, каркас очереди джоб + SSE
//! GET /jobs/{id}/events. GPU-эндпоинты (analyze/render/preview/patch и т.д.) — каркас на следующие
//! раунды; их карта в docs/PORT-CONTRACT.md.

mod analyze;
mod jobs;
mod media;
mod patch;
mod spa;
mod translate;

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
}

/// Корень моделей: env DUBENGINE_MODELS_ROOT, иначе <repo_root>/models.
fn models_root(repo_root: &Path) -> PathBuf {
    std::env::var("DUBENGINE_MODELS_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| repo_root.join("models"))
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
        AppState {
            repo_root,
            workspace,
            web_root,
            opts: Arc::new(EngineOpts::default()),
            jobs: JobQueue::new(),
            tdt_dir,
            sortformer_onnx,
            llama_bin,
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
        .route("/projects", post(create_project))
        .route("/projects/{pid}", get(get_project).patch(patch_project))
        .route("/projects/{pid}/analyze", post(analyze_project))
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
