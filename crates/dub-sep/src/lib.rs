//! dub-sep — вокал/инструментал сепарация (порт dubengine/separate.py).
//! Движок: **Mel-Band Roformer voc_fv6-Q8_0** через
//! нативный сайдкар **BSRoformer.cpp** (C++/ggml, CUDA). Эталон интеграции — voiceclean.rs из
//! Higgs-Ultimate (CLI `<model.gguf> <in.wav> <out.wav>`, вход 44.1кГц, CREATE_NO_WINDOW, cwd=папка
//! движка ради ggml-DLL).
//!
//! CLI отдаёт ТОЛЬКО вокал-стем (в модели num_stems=1). Инструментал получаем как разницу
//! `mix − vocals` во временной области — выход сепаратора выровнен по семплам со входом (та же длина,
//! 44.1кГц), потому вычитание сэмпл-в-сэмпл корректно (стандартная практика для vocal-моделей).
//!
//! Интерфейс: `separate(mix_wav, out_dir) -> SepResult { vocals, instrumental }`. Обе дорожки —
//! WAV float32 44.1кГц, как выдаёт движок; downstream (analyze audio-context / рендер-фон) читает их
//! напрямую.

use std::path::{Path, PathBuf};
use std::process::Command;

mod wav;

#[derive(Debug, thiserror::Error)]
pub enum SepError {
    #[error("движок сепарации не найден: {0}")]
    EngineMissing(PathBuf),
    #[error("модель сепарации не найдена: {0}")]
    ModelMissing(PathBuf),
    #[error("запуск движка: {0}")]
    Spawn(String),
    #[error("движок завершился с ошибкой: {0}")]
    EngineFailed(String),
    #[error("движок не создал вокал-стем: {0}")]
    NoOutput(PathBuf),
    #[error("аудио I/O: {0}")]
    Wav(String),
}

/// Результат сепарации: пути к вокалу и инструменталу (оба WAV, 44.1кГц).
#[derive(Debug, Clone)]
pub struct SepResult {
    pub vocals: PathBuf,
    pub instrumental: PathBuf,
}

/// Сайдкар-CLI движка (Windows). Рядом лежат ggml*.dll — их подхватывает загрузчик из cwd движка.
pub const ENGINE_CLI_FILE: &str = "bs_roformer-cli.exe";
/// GGUF-модель вокал-сепарации по умолчанию (Mel-Band Roformer voc_fv6, Q8_0).
pub const MODEL_FILE: &str = "voc_fv6-Q8_0.gguf";

/// Резолв каталога движка: env DUB_STUDIO_BSROFORMER_DIR, иначе <repo>/tools/bsroformer.
pub fn engine_dir(repo_root: &Path) -> PathBuf {
    std::env::var("DUB_STUDIO_BSROFORMER_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| repo_root.join("tools").join("bsroformer"))
}

/// Резолв GGUF-модели: env DUB_STUDIO_BSROFORMER_MODEL, иначе <repo>/models/bsroformer/<MODEL_FILE>.
pub fn model_path(repo_root: &Path) -> PathBuf {
    std::env::var("DUB_STUDIO_BSROFORMER_MODEL")
        .map(PathBuf::from)
        .unwrap_or_else(|_| repo_root.join("models").join("bsroformer").join(MODEL_FILE))
}

/// Установлены ли обе части (движок + модель) — для UI/фейл-сейф в пайплайне.
pub fn is_installed(repo_root: &Path) -> bool {
    engine_dir(repo_root).join(ENGINE_CLI_FILE).is_file() && model_path(repo_root).is_file()
}

/// Разделить mix (WAV 44.1кГц) на вокал + инструментал. `out_dir` — куда положить `vocals.wav` и
/// `instrumental.wav` (порт `separate.split` контракта). `cli` — путь к bs_roformer-cli.exe,
/// `model` — GGUF. Движок пишет вокал; инструментал считается как mix − vocals во времени.
pub fn separate(
    mix_wav: &Path,
    out_dir: &Path,
    cli: &Path,
    model: &Path,
) -> Result<SepResult, SepError> {
    if !cli.is_file() {
        return Err(SepError::EngineMissing(cli.to_path_buf()));
    }
    if !model.is_file() {
        return Err(SepError::ModelMissing(model.to_path_buf()));
    }
    std::fs::create_dir_all(out_dir).map_err(|e| SepError::Wav(e.to_string()))?;
    let vocals = out_dir.join("vocals.wav");
    let instrumental = out_dir.join("instrumental.wav");

    run_cli(cli, model, mix_wav, &vocals)?;
    if !vocals.is_file() {
        return Err(SepError::NoOutput(vocals));
    }

    // Инструментал = mix − vocals во временной области. Читаем оба, вычитаем сэмпл-в-сэмпл
    // (движок гарантирует ту же длину/частоту), пишем WAV float32.
    let mix = wav::read_f32(mix_wav).map_err(SepError::Wav)?;
    let voc = wav::read_f32(&vocals).map_err(SepError::Wav)?;
    let inst = subtract(&mix, &voc);
    wav::write_f32(&instrumental, &inst).map_err(SepError::Wav)?;

    Ok(SepResult { vocals, instrumental })
}

/// Разница двух дорожек: instrumental[n] = mix[n] − vocals[n]. Каналы/частота берутся из mix; если
/// длины расходятся (крайний случай), выравниваем по минимуму (хвост-остаток mix оставляем как есть).
fn subtract(mix: &wav::Audio, voc: &wav::Audio) -> wav::Audio {
    let ch = mix.channels.max(1);
    let n = mix.data.len().min(voc.data.len());
    let mut out = mix.data.clone();
    for i in 0..n {
        out[i] = mix.data[i] - voc.data[i];
    }
    wav::Audio {
        sample_rate: mix.sample_rate,
        channels: ch,
        data: out,
    }
}

/// Запустить сайдкар без окна консоли, cwd=папка движка (ради ggml-DLL), дождаться, проверить код.
/// Эталон — voiceclean.rs::run_cli из Higgs-Ultimate.
fn run_cli(cli: &Path, model: &Path, input: &Path, output: &Path) -> Result<(), SepError> {
    // cwd меняем на папку движка (ради ggml-DLL), потому ВСЕ пути аргументов делаем абсолютными —
    // иначе относительный путь модели/входа резолвится от чужого cwd и «файл не найден».
    let abs = |p: &Path| -> PathBuf {
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|c| c.join(p))
                .unwrap_or_else(|_| p.to_path_buf())
        }
    };
    let mut cmd = Command::new(abs(cli));
    cmd.arg(abs(model)).arg(abs(input)).arg(abs(output));
    if let Some(dir) = cli.parent() {
        cmd.current_dir(abs(dir));
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let out = cmd.output().map_err(|e| SepError::Spawn(e.to_string()))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let tail: Vec<&str> = stderr
            .lines()
            .chain(stdout.lines())
            .rev()
            .take(8)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        return Err(SepError::EngineFailed(format!(
            "код {:?}: {}",
            out.status.code(),
            tail.join(" | ")
        )));
    }
    Ok(())
}
