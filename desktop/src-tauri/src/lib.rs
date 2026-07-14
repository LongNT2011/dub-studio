//! Tauri-оболочка Dub Studio. Ничего тяжёлого сама не делает: поднимает нативный `dub-server`
//! (axum, тот же REST/SSE-контракт, что бэкенд-питон) на 127.0.0.1:<свободный порт> и открывает
//! окно на этот URL. Сервер сам раздаёт SPA (frontend/dist) и API на одном origin — фронт работает
//! с относительными путями без правок.
//!
//! Портативность взята из эталона Higgs-Ultimate (desktop/src-tauri/src/lib.rs):
//! app_root_dir = каталог рядом с exe; WEBVIEW2_USER_DATA_FOLDER и рантайм-модели держим там же.

use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::path::PathBuf;
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
/// В портативной сборке возвращаем каталог рядом с exe (там лежат frontend/, models/, fonts/).
fn resolve_repo_root() -> PathBuf {
    // Явное переопределение (dev / тесты).
    if let Ok(r) = std::env::var("DUB_STUDIO_ROOT") {
        return PathBuf::from(r);
    }
    let exe_dir = app_root_dir();
    // Портативная раскладка: ресурсы (frontend/models) лежат рядом с оболочкой (dub-server встроен в exe).
    if exe_dir.join("frontend").is_dir() && exe_dir.join("models").is_dir() {
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

/// Прописать ORT_DYLIB_PATH (onnxruntime 1.24) в окружение ПРОЦЕССА до старта сервера (dub-asr трогает ort
/// лениво; PATH под движок/инструменты добавит augment_path_for_tools внутри serve_blocking).
fn setup_server_env(repo_root: &PathBuf) {
    if std::env::var_os("ORT_DYLIB_PATH").is_none() {
        for cand in [
            app_root_dir().join("onnxruntime.dll"),
            repo_root.join("models").join("runtime").join("onnxruntime-1.24.dll"),
            repo_root.join("models").join("runtime").join("onnxruntime.dll"),
        ] {
            if cand.is_file() {
                std::env::set_var("ORT_DYLIB_PATH", cand);
                break;
            }
        }
    }
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
    setup_server_env(&repo_root);

    // axum-бэкенд поднимается В ЭТОМ ЖЕ процессе на фоновом потоке — ОДИН exe, без dub-server.exe-сайдкара.
    // Поток-демон: живёт до выхода процесса, отдельно убивать не нужно (нет дочернего процесса).
    let root = repo_root.clone();
    std::thread::spawn(move || {
        if let Err(e) = dub_server::serve_blocking(&root, port) {
            eprintln!("встроенный dub-server упал: {e}");
        }
    });

    // Ждём готовности сервера, чтобы окно не открылось на пустоту.
    if !wait_until_ready(port, Duration::from_secs(30)) {
        eprintln!("встроенный dub-server не поднялся на 127.0.0.1:{port} за 30с");
    }

    let url = format!("http://127.0.0.1:{port}/");

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
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
}
