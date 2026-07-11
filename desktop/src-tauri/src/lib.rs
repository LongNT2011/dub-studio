//! Tauri-оболочка Dub Studio. Ничего тяжёлого сама не делает: поднимает нативный `dub-server`
//! (axum, тот же REST/SSE-контракт, что бэкенд-питон) на 127.0.0.1:<свободный порт> и открывает
//! окно на этот URL. Сервер сам раздаёт SPA (frontend/dist) и API на одном origin — фронт работает
//! с относительными путями без правок.
//!
//! Портативность взята из эталона Higgs-Ultimate (desktop/src-tauri/src/lib.rs):
//! app_root_dir = каталог рядом с exe; WEBVIEW2_USER_DATA_FOLDER и рантайм-модели держим там же.

use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{WebviewUrl, WebviewWindowBuilder};

/// Каталог рядом с exe (портативная установка). Дев-режим: корень репозитория.
fn app_root_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Найти корень репо в dev (…/desktop/src-tauri/target/<profile>/exe -> вверх до dub-studio).
/// В портативной сборке возвращаем каталог рядом с exe (там лежат dub-server.exe, models/, frontend/).
fn resolve_repo_root() -> PathBuf {
    // Явное переопределение (dev / тесты).
    if let Ok(r) = std::env::var("DUB_STUDIO_ROOT") {
        return PathBuf::from(r);
    }
    let exe_dir = app_root_dir();
    // Портативная раскладка: dub-server.exe лежит рядом с оболочкой.
    if exe_dir.join("dub-server.exe").is_file() || exe_dir.join("dub-server").is_file() {
        return exe_dir;
    }
    // Dev: exe в …/desktop/src-tauri/target/<profile>/. Поднимаемся до каталога с crates/.
    let mut d = exe_dir.as_path();
    for _ in 0..6 {
        if d.join("crates").is_dir() && d.join("frontend").is_dir() {
            return d.to_path_buf();
        }
        match d.parent() {
            Some(p) => d = p,
            None => break,
        }
    }
    exe_dir
}

/// Путь к бинарю dub-server: рядом с exe (портатив) или в target/<profile> репо (dev).
fn resolve_server_bin(repo_root: &PathBuf) -> PathBuf {
    let name = if cfg!(windows) { "dub-server.exe" } else { "dub-server" };
    let near = app_root_dir().join(name);
    if near.is_file() {
        return near;
    }
    for profile in ["release", "debug"] {
        let cand = repo_root.join("target").join(profile).join(name);
        if cand.is_file() {
            return cand;
        }
    }
    near
}

/// Занять свободный TCP-порт на 127.0.0.1 (ядро выдаёт порт 0 -> читаем реальный, отпускаем).
fn pick_free_port() -> std::io::Result<u16> {
    let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))?;
    Ok(listener.local_addr()?.port())
}

/// Дождаться, пока сервер начнёт принимать соединения (или таймаут).
fn wait_until_ready(port: u16, timeout: Duration) -> bool {
    let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
    let start = Instant::now();
    while start.elapsed() < timeout {
        if TcpStream::connect_timeout(&addr.into(), Duration::from_millis(200)).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(120));
    }
    false
}

/// Ручка дочернего сервера — убиваем его при выходе приложения.
struct ServerProc(Mutex<Option<Child>>);

impl Drop for ServerProc {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.0.lock() {
            if let Some(mut child) = guard.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

/// Поднять dub-server как дочерний процесс на выбранном порту. Прокидываем окружение: корень репо,
/// порт (DUB_STUDIO_PORT), путь к onnxruntime 1.24 и CUDA-DLL движка (портативно рядом с exe).
fn spawn_server(repo_root: &PathBuf, port: u16) -> std::io::Result<Child> {
    let bin = resolve_server_bin(repo_root);
    let mut cmd = Command::new(&bin);
    cmd.env("DUB_STUDIO_ROOT", repo_root)
        .env("DUB_STUDIO_PORT", port.to_string())
        .env("DUB_STUDIO_HOST", "127.0.0.1");

    // onnxruntime 1.24: рядом с exe (портатив) или в models/runtime (dev).
    if std::env::var_os("ORT_DYLIB_PATH").is_none() {
        for cand in [
            app_root_dir().join("onnxruntime.dll"),
            repo_root.join("models").join("runtime").join("onnxruntime-1.24.dll"),
            repo_root.join("models").join("runtime").join("onnxruntime.dll"),
        ] {
            if cand.is_file() {
                cmd.env("ORT_DYLIB_PATH", cand);
                break;
            }
        }
    }

    // PATH += каталог движка Higgs (cuda/ggml/vcruntime DLL) — движок ищет зависимости в PATH.
    let engine_dir = repo_root.join("models").join("higgs-engine");
    if engine_dir.is_dir() {
        let cur = std::env::var("PATH").unwrap_or_default();
        cmd.env("PATH", format!("{};{}", engine_dir.display(), cur));
    }

    cmd.spawn()
}

pub fn run() {
    // Портатив: состояние WebView2 (localStorage) держим рядом с exe, а не в профиле пользователя.
    if std::env::var_os("WEBVIEW2_USER_DATA_FOLDER").is_none() {
        std::env::set_var(
            "WEBVIEW2_USER_DATA_FOLDER",
            app_root_dir().join("webview-data"),
        );
    }

    let repo_root = resolve_repo_root();
    let port = pick_free_port().unwrap_or(8765);

    let child = match spawn_server(&repo_root, port) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("не удалось запустить dub-server: {e}");
            std::process::exit(1);
        }
    };
    let server = ServerProc(Mutex::new(Some(child)));

    // Ждём готовности сервера, чтобы окно не открылось на пустоту.
    if !wait_until_ready(port, Duration::from_secs(30)) {
        eprintln!("dub-server не поднялся на 127.0.0.1:{port} за 30с");
    }

    let url = format!("http://127.0.0.1:{port}/");

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(server)
        .setup(move |app| {
            let win = WebviewWindowBuilder::new(
                app,
                "main",
                WebviewUrl::External(url.parse().expect("валидный URL")),
            )
            .title("Dub Studio")
            .inner_size(1280.0, 860.0)
            .min_inner_size(1180.0, 720.0)
            .resizable(true)
            .build()?;
            let _ = win;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("ошибка запуска Tauri");
    // ServerProc живёт в состоянии Tauri (.manage) -> его Drop убивает dub-server при выходе.
}
