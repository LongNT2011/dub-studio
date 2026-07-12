//! setup — «первый запуск»: манифест всех внешних компонентов (модели, движки-сайдкары, системные
//! библиотеки) + диагностика их наличия на диске + фоновая автозакачка одной кнопкой: свести
//! ручную установку к
//! единственному шагу (драйвер NVIDIA), убрав из README требование ставить CUDA Toolkit, VC++ и
//! качать веса вручную.
//!
//! Эталон паттернов — Higgs-Ultimate desktop/src-tauri/src/envdeps.rs (env_check / download_env_deps /
//! extract_dlls_from_wheel) и download.rs (прогресс-машина). Здесь адаптировано под axum-сервер: тот же
//! job-контракт SSE, что analyze/render, а прогресс идёт из download-loop в колбэк джобы.
//!
//! Классы источников (все URL — ровно те, что уже использованы; см. crates/README.md, PORT-CONTRACT.md,
//! Higgs voiceclean.rs):
//!   • модели — прямые файлы HF (higgs-q8_0/*, gemma-4 + mmproj, parakeet-tdt int8, sortformer, roformer
//!     voc_fv6-Q8_0);
//!   • сайдкары/движки — zip-релизы GitHub (BSRoformer.cpp, llama.cpp win-cuda-13.3, onnxruntime 1.24.2,
//!     ffmpeg BtbN) + audiocpp_engine.dll (HF);
//!   • CUDA-runtime — PyPI-wheel'ы NVIDIA (cudart 13.3.29 / cublas 13.6.0.2), распаковка *.dll плоско;
//!   • VC++ runtime + OCR-модели — БАНДЛ (кладутся в релиз рядом с exe, как VC++ в Higgs); не качаются,
//!     но статус показываем;
//!   • драйвер NVIDIA — детект (nvcuda.dll), «скачивание» = открыть сайт (кнопка во фронте).

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{json, Value};

// ── Тип файла-члена компонента ──────────────────────────────────────────────

/// Одна закачиваемая единица компонента: URL + относительный путь назначения (от repo_root) + ожидаемый
/// размер (для проверки «докачано целиком» и для UI). Для zip/wheel — это временный архив, распаковка
/// раскладывает содержимое (см. `Extract`).
#[derive(Clone, Debug)]
pub struct FileSpec {
    pub url: &'static str,
    /// Куда лечь ФАЙЛУ (для прямых файлов) ИЛИ временный путь архива (для zip/wheel). Относительно repo_root.
    pub dest_rel: &'static str,
    /// Ожидаемый размер в байтах (0 = неизвестно/rolling-релиз). Для файлов-моделей — точный (сверен с диском).
    pub size: u64,
    pub extract: Extract,
}

/// Что делать со скачанным файлом.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Extract {
    /// Прямой файл — оставить как есть (dest_rel = финальный путь).
    None,
    /// zip: распаковать плоско (только имена файлов) в каталог dest_rel-родителя, затем удалить архив.
    ZipFlat,
    /// zip: отобрать конкретные файлы по имени листа (ffmpeg.exe/ffprobe.exe) и положить плоско в каталог.
    ZipPick,
    /// zip: распаковать ВЕСЬ архив с сохранением поддерева в каталог. Для onnxruntime — чтобы получить
    /// `onnxruntime-win-x64-1.24.2/lib/onnxruntime.dll` ровно там, где его ищет dub-asr::ensure_ort_dylib.
    ZipTree,
    /// wheel (zip): достать все *.dll плоско в каталог, затем удалить архив (CUDA runtime).
    WheelDlls,
}

/// Обязательность компонента для запуска пайплайна.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Requirement {
    /// Без него analyze/render не работает — блокирует «первый запуск».
    Required,
    /// Улучшает результат, но пайплайн деградирует gracefully (напр. OCR-блюр).
    Recommended,
    /// Альтернативный вариант/квант (качается по выбору в настройках, не преселектится, не гейтит).
    Optional,
}

/// Как компонент попадает на диск.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Delivery {
    /// Качается сетью (есть file_specs с URL).
    Download,
    /// Идёт в комплекте релиза рядом с exe (VC++, OCR-модели). Не качаем; если нет — просим переустановить.
    Bundled,
    /// Часть системы, ставится вне приложения (драйвер NVIDIA). «Скачать» = открыть сайт.
    External,
}

/// Описание одного компонента манифеста.
#[derive(Clone, Debug)]
pub struct Component {
    pub id: &'static str,
    /// Человекочитаемое имя (i18n-ключ на фронте — `setup.comp.<id>.name`; здесь дефолт-строка RU для API).
    pub name: &'static str,
    /// Назначение (что сломается без него).
    pub purpose: &'static str,
    pub requirement: Requirement,
    pub delivery: Delivery,
    /// Совокупный размер компонента, байт (сумма файлов; для rolling-релизов — оценка).
    pub size: u64,
    /// Файлы к закачке (для Delivery::Download). Пусто у Bundled/External.
    pub files: &'static [FileSpec],
    /// Пути-«маркеры» существования (относительно repo_root). Компонент installed, когда ВСЕ маркеры на
    /// месте и (для файлов с известным размером) их размер совпадает.
    pub markers: &'static [Marker],
    /// URL внешней страницы (для Delivery::External — сайт драйвера).
    pub external_url: Option<&'static str>,
}

/// Маркер наличия: путь + опц. минимальный размер (0 = только существование).
#[derive(Clone, Copy, Debug)]
pub struct Marker {
    pub rel: &'static str,
    /// Ожидаемый точный размер (0 = не проверять). Файл «целый», если размер == expect (для докачки/резюме).
    pub expect: u64,
}

// ═══════════════════════════════════════════════════════════════════════════
//  URL-константы (источник истины; сверены HEAD-запросами, размеры = байт-в-байт с диском)
// ═══════════════════════════════════════════════════════════════════════════

// HF: Sortformer v2 диаризация (altunenes/parakeet-rs).
const HF_SORTFORMER: &str =
    "https://huggingface.co/altunenes/parakeet-rs/resolve/main/diar_streaming_sortformer_4spk-v2.onnx";
// HF: Mel-Band Roformer voc_fv6-Q8_0 (chenmozhijin/BSRoformer-GGUF).
const HF_ROFORMER: &str = "https://huggingface.co/chenmozhijin/BSRoformer-GGUF/resolve/main/GaboxR67/MelBandRoformers/melbandroformers/vocals/voc_fv6-Q8_0.gguf";
// GitHub: BSRoformer.cpp движок win-cuda-13.1.0 zip (chenmozhijin/BSRoformer.cpp v0.1.0).
const GH_BSROFORMER_ENGINE: &str =
    "https://github.com/chenmozhijin/BSRoformer.cpp/releases/download/v0.1.0/BSRoformer-windows-cuda-13.1.0.zip";
// GitHub: llama.cpp win-cuda-13.3 (ggml-org/llama.cpp; пин на стабильный билд + cudart-компаньон).
const GH_LLAMA_BUILD: &str = "b9966";
const GH_LLAMA: &str =
    "https://github.com/ggml-org/llama.cpp/releases/download/b9966/llama-b9966-bin-win-cuda-13.3-x64.zip";
// GitHub: onnxruntime 1.24.2 win-x64 (microsoft/onnxruntime) — строго 1.24.2 (rc.12 ABI; иначе дедлок).
const GH_ORT: &str =
    "https://github.com/microsoft/onnxruntime/releases/download/v1.24.2/onnxruntime-win-x64-1.24.2.zip";
// GitHub: ffmpeg static win64 GPL (BtbN/FFmpeg-Builds) — тот же источник, что install.bat.
const GH_FFMPEG: &str =
    "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl.zip";
// PyPI-wheel'ы NVIDIA CUDA 13 runtime (те же, что Higgs envdeps.rs — байт-в-байт с локальным CUDA 13.3).
const WHEEL_CUDART: &str = "https://files.pythonhosted.org/packages/d2/27/b53a5e0397842a5c11f0e1a39d4e5b2f22638a4126e83b3c4e196f62c969/nvidia_cuda_runtime-13.3.29-py3-none-win_amd64.whl";
const WHEEL_CUBLAS: &str = "https://files.pythonhosted.org/packages/08/8f/890a96ea1ff615100296977cce23296052dcb8c114d4e451201ec39df9bf/nvidia_cublas-13.6.0.2-py3-none-win_amd64.whl";
// Страница драйверов NVIDIA (кнопка «Открыть сайт» — драйвер DLL-кой не ставится).
pub const NVIDIA_DRIVER_URL: &str = "https://www.nvidia.com/Download/index.aspx";

// ═══════════════════════════════════════════════════════════════════════════
//  МАНИФЕСТ
// ═══════════════════════════════════════════════════════════════════════════

/// Полный список компонентов. Порядок = порядок показа в панели «Первый запуск».
pub fn manifest() -> Vec<Component> {
    vec![
        // ── МОДЕЛИ ──────────────────────────────────────────────────────────
        Component {
            id: "higgs",
            name: "Higgs Audio v3 (Q8_0)",
            purpose: "Синтез дубляжа и клон голоса (TTS)",
            requirement: Requirement::Required,
            delivery: Delivery::Download,
            size: 5_534_363_733,
            files: &[
                FileSpec { url: "https://huggingface.co/drbaph/Higgs-Audio-v3-Studio/resolve/main/models/higgs-q8_0/q8_0.gguf", dest_rel: "models/higgs-q8_0/q8_0.gguf", size: 5_519_235_296, extract: Extract::None },
                FileSpec { url: "https://huggingface.co/drbaph/Higgs-Audio-v3-Studio/resolve/main/models/higgs-q8_0/config.json", dest_rel: "models/higgs-q8_0/config.json", size: 2_755, extract: Extract::None },
                FileSpec { url: "https://huggingface.co/drbaph/Higgs-Audio-v3-Studio/resolve/main/models/higgs-q8_0/chat_template.jinja", dest_rel: "models/higgs-q8_0/chat_template.jinja", size: 2_427, extract: Extract::None },
                FileSpec { url: "https://huggingface.co/drbaph/Higgs-Audio-v3-Studio/resolve/main/models/higgs-q8_0/tokenizer.json", dest_rel: "models/higgs-q8_0/tokenizer.json", size: 11_433_924, extract: Extract::None },
                FileSpec { url: "https://huggingface.co/drbaph/Higgs-Audio-v3-Studio/resolve/main/models/higgs-q8_0/tokenizer_config.json", dest_rel: "models/higgs-q8_0/tokenizer_config.json", size: 1_937, extract: Extract::None },
                FileSpec { url: "https://huggingface.co/drbaph/Higgs-Audio-v3-Studio/resolve/main/models/higgs-q8_0/higgs_audio_v2_tokenizer_config.json", dest_rel: "models/higgs-q8_0/higgs_audio_v2_tokenizer_config.json", size: 2_251, extract: Extract::None },
            ],
            markers: &[
                Marker { rel: "models/higgs-q8_0/q8_0.gguf", expect: 5_519_235_296 },
                Marker { rel: "models/higgs-q8_0/config.json", expect: 2_755 },
                Marker { rel: "models/higgs-q8_0/tokenizer.json", expect: 11_433_924 },
            ],
            external_url: None,
        },
        Component {
            id: "higgs-engine",
            name: "Higgs движок (audiocpp_engine.dll)",
            purpose: "Нативный TTS-движок Higgs (C-ABI)",
            requirement: Requirement::Required,
            delivery: Delivery::Download,
            size: 71_727_104,
            files: &[
                FileSpec { url: "https://huggingface.co/drbaph/Higgs-Audio-v3-Studio/resolve/main/engines/audiocpp_engine.dll", dest_rel: "models/higgs-engine/audiocpp_engine.dll", size: 71_727_104, extract: Extract::None },
            ],
            markers: &[Marker { rel: "models/higgs-engine/audiocpp_engine.dll", expect: 71_727_104 }],
            external_url: None,
        },
        Component {
            id: "gemma",
            name: "Gemma-4 12B QAT q4_0 + vision",
            purpose: "Перевод и vision-оркестратор субтитров/титров",
            requirement: Requirement::Required,
            delivery: Delivery::Download,
            size: 7_150_992_992,
            files: &[
                FileSpec { url: "https://huggingface.co/google/gemma-4-12b-it-qat-q4_0-gguf/resolve/main/gemma-4-12b-it-qat-q4_0.gguf", dest_rel: "models/mt/gemma-4-12b-it-qat-q4_0.gguf", size: 6_975_877_728, extract: Extract::None },
                FileSpec { url: "https://huggingface.co/google/gemma-4-12b-it-qat-q4_0-gguf/resolve/main/mmproj-gemma-4-12b-it-qat-q4_0.gguf", dest_rel: "models/mt/mmproj-gemma-4-12b-it-qat-q4_0.gguf", size: 175_115_264, extract: Extract::None },
            ],
            markers: &[
                Marker { rel: "models/mt/gemma-4-12b-it-qat-q4_0.gguf", expect: 6_975_877_728 },
                Marker { rel: "models/mt/mmproj-gemma-4-12b-it-qat-q4_0.gguf", expect: 175_115_264 },
            ],
            external_url: None,
        },
        // Альтернативные кванты Gemma (выбор в настройках; тяжелее q4_0, чуть точнее). Свой mmproj на квант.
        Component {
            id: "gemma-q5_0",
            name: "Gemma-4 12B QAT (q5_0) + vision",
            purpose: "Перевод и vision — вариант точнее q4_0",
            requirement: Requirement::Optional,
            delivery: Delivery::Download,
            size: 8_600_000_000,
            files: &[
                FileSpec { url: "https://huggingface.co/google/gemma-4-12b-it-qat-q4_0-gguf/resolve/main/gemma-4-12b-it-qat-q5_0.gguf", dest_rel: "models/mt-q5_0/gemma-4-12b-it-qat-q5_0.gguf", size: 0, extract: Extract::None },
                FileSpec { url: "https://huggingface.co/google/gemma-4-12b-it-qat-q4_0-gguf/resolve/main/mmproj-gemma-4-12b-it-qat-q5_0.gguf", dest_rel: "models/mt-q5_0/mmproj-gemma-4-12b-it-qat-q5_0.gguf", size: 0, extract: Extract::None },
            ],
            markers: &[Marker { rel: "models/mt-q5_0/gemma-4-12b-it-qat-q5_0.gguf", expect: 0 }, Marker { rel: "models/mt-q5_0/mmproj-gemma-4-12b-it-qat-q5_0.gguf", expect: 0 }],
            external_url: None,
        },
        Component {
            id: "gemma-q6_k",
            name: "Gemma-4 12B QAT (q6_k) + vision",
            purpose: "Перевод и vision — ещё точнее",
            requirement: Requirement::Optional,
            delivery: Delivery::Download,
            size: 9_900_000_000,
            files: &[
                FileSpec { url: "https://huggingface.co/google/gemma-4-12b-it-qat-q4_0-gguf/resolve/main/gemma-4-12b-it-qat-q6_k.gguf", dest_rel: "models/mt-q6_k/gemma-4-12b-it-qat-q6_k.gguf", size: 0, extract: Extract::None },
                FileSpec { url: "https://huggingface.co/google/gemma-4-12b-it-qat-q4_0-gguf/resolve/main/mmproj-gemma-4-12b-it-qat-q6_k.gguf", dest_rel: "models/mt-q6_k/mmproj-gemma-4-12b-it-qat-q6_k.gguf", size: 0, extract: Extract::None },
            ],
            markers: &[Marker { rel: "models/mt-q6_k/gemma-4-12b-it-qat-q6_k.gguf", expect: 0 }, Marker { rel: "models/mt-q6_k/mmproj-gemma-4-12b-it-qat-q6_k.gguf", expect: 0 }],
            external_url: None,
        },
        Component {
            id: "gemma-q8_0",
            name: "Gemma-4 12B QAT (q8_0) + vision",
            purpose: "Перевод и vision — максимальная точность",
            requirement: Requirement::Optional,
            delivery: Delivery::Download,
            size: 12_800_000_000,
            files: &[
                FileSpec { url: "https://huggingface.co/google/gemma-4-12b-it-qat-q4_0-gguf/resolve/main/gemma-4-12b-it-qat-q8_0.gguf", dest_rel: "models/mt-q8_0/gemma-4-12b-it-qat-q8_0.gguf", size: 0, extract: Extract::None },
                FileSpec { url: "https://huggingface.co/google/gemma-4-12b-it-qat-q4_0-gguf/resolve/main/mmproj-gemma-4-12b-it-qat-q8_0.gguf", dest_rel: "models/mt-q8_0/mmproj-gemma-4-12b-it-qat-q8_0.gguf", size: 0, extract: Extract::None },
            ],
            markers: &[Marker { rel: "models/mt-q8_0/gemma-4-12b-it-qat-q8_0.gguf", expect: 0 }, Marker { rel: "models/mt-q8_0/mmproj-gemma-4-12b-it-qat-q8_0.gguf", expect: 0 }],
            external_url: None,
        },
        Component {
            id: "parakeet",
            name: "Parakeet-TDT 0.6B v3 (int8)",
            purpose: "Распознавание речи со словными таймстемпами (ASR)",
            requirement: Requirement::Required,
            delivery: Delivery::Download,
            size: 688_819_567,
            files: &[
                FileSpec { url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/encoder-model.int8.onnx", dest_rel: "models/tdt/encoder-model.int8.onnx", size: 652_183_999, extract: Extract::None },
                FileSpec { url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/decoder_joint-model.int8.onnx", dest_rel: "models/tdt/decoder_joint-model.int8.onnx", size: 18_202_004, extract: Extract::None },
                FileSpec { url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/nemo128.onnx", dest_rel: "models/tdt/nemo128.onnx", size: 139_764, extract: Extract::None },
                FileSpec { url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/vocab.txt", dest_rel: "models/tdt/vocab.txt", size: 93_939, extract: Extract::None },
                FileSpec { url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/config.json", dest_rel: "models/tdt/config.json", size: 97, extract: Extract::None },
            ],
            markers: &[
                Marker { rel: "models/tdt/encoder-model.int8.onnx", expect: 652_183_999 },
                Marker { rel: "models/tdt/decoder_joint-model.int8.onnx", expect: 18_202_004 },
                Marker { rel: "models/tdt/vocab.txt", expect: 93_939 },
            ],
            external_url: None,
        },
        // Альтернативные кванты TTS Higgs (выбор в настройках; своя папка на квант, aux те же).
        Component {
            id: "higgs-q6_k",
            name: "Higgs Audio v3 (Q6_K)",
            purpose: "Синтез дубляжа и клон голоса (TTS) — вариант полегче Q8_0",
            requirement: Requirement::Optional,
            delivery: Delivery::Download,
            size: 5_035_000_000,
            files: &[
                FileSpec { url: "https://huggingface.co/drbaph/Higgs-Audio-v3-Studio/resolve/main/models/higgs-q6_k/q6_k.gguf", dest_rel: "models/higgs-q6_k/q6_k.gguf", size: 5_023_637_248, extract: Extract::None },
                FileSpec { url: "https://huggingface.co/drbaph/Higgs-Audio-v3-Studio/resolve/main/models/higgs-q6_k/config.json", dest_rel: "models/higgs-q6_k/config.json", size: 0, extract: Extract::None },
                FileSpec { url: "https://huggingface.co/drbaph/Higgs-Audio-v3-Studio/resolve/main/models/higgs-q6_k/chat_template.jinja", dest_rel: "models/higgs-q6_k/chat_template.jinja", size: 0, extract: Extract::None },
                FileSpec { url: "https://huggingface.co/drbaph/Higgs-Audio-v3-Studio/resolve/main/models/higgs-q6_k/tokenizer.json", dest_rel: "models/higgs-q6_k/tokenizer.json", size: 11_433_924, extract: Extract::None },
                FileSpec { url: "https://huggingface.co/drbaph/Higgs-Audio-v3-Studio/resolve/main/models/higgs-q6_k/tokenizer_config.json", dest_rel: "models/higgs-q6_k/tokenizer_config.json", size: 0, extract: Extract::None },
                FileSpec { url: "https://huggingface.co/drbaph/Higgs-Audio-v3-Studio/resolve/main/models/higgs-q6_k/higgs_audio_v2_tokenizer_config.json", dest_rel: "models/higgs-q6_k/higgs_audio_v2_tokenizer_config.json", size: 0, extract: Extract::None },
            ],
            markers: &[Marker { rel: "models/higgs-q6_k/q6_k.gguf", expect: 5_023_637_248 }, Marker { rel: "models/higgs-q6_k/tokenizer.json", expect: 0 }],
            external_url: None,
        },
        Component {
            id: "higgs-q4_k_m",
            name: "Higgs Audio v3 (Q4_K_M)",
            purpose: "Синтез дубляжа и клон голоса (TTS) — самый лёгкий вариант",
            requirement: Requirement::Optional,
            delivery: Delivery::Download,
            size: 4_098_000_000,
            files: &[
                FileSpec { url: "https://huggingface.co/drbaph/Higgs-Audio-v3-Studio/resolve/main/models/higgs-q4_k_m/q4_k_m.gguf", dest_rel: "models/higgs-q4_k_m/q4_k_m.gguf", size: 4_086_922_976, extract: Extract::None },
                FileSpec { url: "https://huggingface.co/drbaph/Higgs-Audio-v3-Studio/resolve/main/models/higgs-q4_k_m/config.json", dest_rel: "models/higgs-q4_k_m/config.json", size: 0, extract: Extract::None },
                FileSpec { url: "https://huggingface.co/drbaph/Higgs-Audio-v3-Studio/resolve/main/models/higgs-q4_k_m/chat_template.jinja", dest_rel: "models/higgs-q4_k_m/chat_template.jinja", size: 0, extract: Extract::None },
                FileSpec { url: "https://huggingface.co/drbaph/Higgs-Audio-v3-Studio/resolve/main/models/higgs-q4_k_m/tokenizer.json", dest_rel: "models/higgs-q4_k_m/tokenizer.json", size: 11_433_924, extract: Extract::None },
                FileSpec { url: "https://huggingface.co/drbaph/Higgs-Audio-v3-Studio/resolve/main/models/higgs-q4_k_m/tokenizer_config.json", dest_rel: "models/higgs-q4_k_m/tokenizer_config.json", size: 0, extract: Extract::None },
                FileSpec { url: "https://huggingface.co/drbaph/Higgs-Audio-v3-Studio/resolve/main/models/higgs-q4_k_m/higgs_audio_v2_tokenizer_config.json", dest_rel: "models/higgs-q4_k_m/higgs_audio_v2_tokenizer_config.json", size: 0, extract: Extract::None },
            ],
            markers: &[Marker { rel: "models/higgs-q4_k_m/q4_k_m.gguf", expect: 4_086_922_976 }, Marker { rel: "models/higgs-q4_k_m/tokenizer.json", expect: 0 }],
            external_url: None,
        },
        // Альтернативный квант ASR: fp32 (точнее, тяжелее int8). Отдельная папка (fp32 приоритетнее int8).
        Component {
            id: "parakeet-fp32",
            name: "Parakeet-TDT 0.6B v3 (fp32)",
            purpose: "Распознавание речи (ASR) — полная точность fp32",
            requirement: Requirement::Optional,
            delivery: Delivery::Download,
            size: 2_560_000_000,
            files: &[
                FileSpec { url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/encoder-model.onnx", dest_rel: "models/tdt-fp32/encoder-model.onnx", size: 41_770_866, extract: Extract::None },
                FileSpec { url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/encoder-model.onnx_data", dest_rel: "models/tdt-fp32/encoder-model.onnx_data", size: 0, extract: Extract::None },
                FileSpec { url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/decoder_joint-model.onnx", dest_rel: "models/tdt-fp32/decoder_joint-model.onnx", size: 72_520_893, extract: Extract::None },
                FileSpec { url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/nemo128.onnx", dest_rel: "models/tdt-fp32/nemo128.onnx", size: 139_764, extract: Extract::None },
                FileSpec { url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/vocab.txt", dest_rel: "models/tdt-fp32/vocab.txt", size: 93_939, extract: Extract::None },
                FileSpec { url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/config.json", dest_rel: "models/tdt-fp32/config.json", size: 0, extract: Extract::None },
            ],
            markers: &[Marker { rel: "models/tdt-fp32/encoder-model.onnx", expect: 41_770_866 }, Marker { rel: "models/tdt-fp32/vocab.txt", expect: 93_939 }],
            external_url: None,
        },
        Component {
            id: "sortformer",
            name: "Sortformer v2 (диаризация)",
            purpose: "Разделение спикеров (кто когда говорит)",
            requirement: Requirement::Recommended,
            delivery: Delivery::Download,
            size: 492_243_002,
            files: &[
                FileSpec { url: HF_SORTFORMER, dest_rel: "models/sortformer/diar_streaming_sortformer_4spk-v2.onnx", size: 492_243_002, extract: Extract::None },
            ],
            markers: &[Marker { rel: "models/sortformer/diar_streaming_sortformer_4spk-v2.onnx", expect: 492_243_002 }],
            external_url: None,
        },
        Component {
            id: "roformer",
            name: "Mel-Band Roformer voc_fv6 (Q8_0)",
            purpose: "Модель вокал/инструментал сепарации",
            requirement: Requirement::Recommended,
            delivery: Delivery::Download,
            size: 251_707_744,
            files: &[
                FileSpec { url: HF_ROFORMER, dest_rel: "models/bsroformer/voc_fv6-Q8_0.gguf", size: 251_707_744, extract: Extract::None },
            ],
            markers: &[Marker { rel: "models/bsroformer/voc_fv6-Q8_0.gguf", expect: 251_707_744 }],
            external_url: None,
        },
        // Альтернативные кванты сепарации (выбор в настройках; лёгкие, качество чуть ниже Q8_0).
        Component {
            id: "roformer-q5",
            name: "Mel-Band Roformer voc_fv6 (Q5_0)",
            purpose: "Сепарация — вариант полегче Q8_0",
            requirement: Requirement::Optional,
            delivery: Delivery::Download,
            size: 167_303_008,
            files: &[
                FileSpec { url: "https://huggingface.co/chenmozhijin/BSRoformer-GGUF/resolve/main/GaboxR67/MelBandRoformers/melbandroformers/vocals/voc_fv6-Q5_0.gguf", dest_rel: "models/bsroformer/voc_fv6-Q5_0.gguf", size: 167_303_008, extract: Extract::None },
            ],
            markers: &[Marker { rel: "models/bsroformer/voc_fv6-Q5_0.gguf", expect: 167_303_008 }],
            external_url: None,
        },
        Component {
            id: "roformer-q4",
            name: "Mel-Band Roformer voc_fv6 (Q4_0)",
            purpose: "Сепарация — самый лёгкий вариант",
            requirement: Requirement::Optional,
            delivery: Delivery::Download,
            size: 139_168_096,
            files: &[
                FileSpec { url: "https://huggingface.co/chenmozhijin/BSRoformer-GGUF/resolve/main/GaboxR67/MelBandRoformers/melbandroformers/vocals/voc_fv6-Q4_0.gguf", dest_rel: "models/bsroformer/voc_fv6-Q4_0.gguf", size: 139_168_096, extract: Extract::None },
            ],
            markers: &[Marker { rel: "models/bsroformer/voc_fv6-Q4_0.gguf", expect: 139_168_096 }],
            external_url: None,
        },
        // ── СAЙДКАРЫ / ДВИЖКИ ───────────────────────────────────────────────
        Component {
            id: "bsroformer-engine",
            name: "BSRoformer.cpp движок (CUDA)",
            purpose: "Нативный движок сепарации (bs_roformer-cli + ggml-CUDA)",
            requirement: Requirement::Recommended,
            delivery: Delivery::Download,
            size: 164_990_561,
            files: &[
                FileSpec { url: GH_BSROFORMER_ENGINE, dest_rel: "tools/bsroformer/_engine.zip", size: 164_990_561, extract: Extract::ZipFlat },
            ],
            markers: &[Marker { rel: "tools/bsroformer/bs_roformer-cli.exe", expect: 0 }],
            external_url: None,
        },
        Component {
            id: "llama",
            name: "llama.cpp сервер (CUDA 13.3)",
            purpose: "Сайдкар-сервер для Gemma (перевод/vision)",
            requirement: Requirement::Required,
            delivery: Delivery::Download,
            // Размер сжатого zip (для прогресса закачки); распакованный footprint ~683 МБ.
            size: 162_331_298,
            files: &[
                FileSpec { url: GH_LLAMA, dest_rel: "tools/llama/_llama.zip", size: 0, extract: Extract::ZipFlat },
            ],
            markers: &[Marker { rel: "tools/llama/llama-server.exe", expect: 0 }],
            external_url: None,
        },
        Component {
            id: "onnxruntime",
            name: "ONNX Runtime 1.24.2",
            purpose: "Рантайм ASR/OCR/диаризации (строго 1.24.2)",
            requirement: Requirement::Required,
            delivery: Delivery::Download,
            size: 74_075_355,
            files: &[
                FileSpec { url: GH_ORT, dest_rel: "models/runtime/_ort.zip", size: 74_075_355, extract: Extract::ZipTree },
            ],
            // dub-asr::ensure_ort_dylib ищет ровно этот путь под models/runtime.
            markers: &[Marker { rel: "models/runtime/onnxruntime-win-x64-1.24.2/lib/onnxruntime.dll", expect: 0 }],
            external_url: None,
        },
        Component {
            id: "ffmpeg",
            name: "FFmpeg (static win64)",
            purpose: "Декод/энкод видео и аудио (NVENC)",
            requirement: Requirement::Required,
            delivery: Delivery::Download,
            size: 168_601_393,
            files: &[
                FileSpec { url: GH_FFMPEG, dest_rel: "tools/ffmpeg/_ffmpeg.zip", size: 0, extract: Extract::ZipPick },
            ],
            markers: &[Marker { rel: "tools/ffmpeg/ffmpeg.exe", expect: 0 }],
            external_url: None,
        },
        // ── СИСТЕМНОЕ ────────────────────────────────────────────────────────
        Component {
            id: "cuda-runtime",
            name: "CUDA 13 runtime (cudart + cuBLAS)",
            purpose: "Редистрибутивные CUDA-DLL для движков (без CUDA Toolkit)",
            requirement: Requirement::Required,
            delivery: Delivery::Download,
            size: 416_407_552,
            files: &[
                FileSpec { url: WHEEL_CUDART, dest_rel: "models/higgs-engine/_cudart.whl", size: 0, extract: Extract::WheelDlls },
                FileSpec { url: WHEEL_CUBLAS, dest_rel: "models/higgs-engine/_cublas.whl", size: 0, extract: Extract::WheelDlls },
            ],
            markers: &[
                Marker { rel: "models/higgs-engine/cudart64_13.dll", expect: 0 },
                Marker { rel: "models/higgs-engine/cublas64_13.dll", expect: 0 },
                Marker { rel: "models/higgs-engine/cublasLt64_13.dll", expect: 0 },
            ],
            external_url: None,
        },
        Component {
            id: "vcruntime",
            name: "Visual C++ Runtime (2015–2022)",
            purpose: "Системные DLL движков (идут в комплекте)",
            requirement: Requirement::Required,
            delivery: Delivery::Bundled,
            size: 1_084_896,
            files: &[],
            markers: &[
                Marker { rel: "models/higgs-engine/MSVCP140.dll", expect: 0 },
                Marker { rel: "models/higgs-engine/VCRUNTIME140.dll", expect: 0 },
                Marker { rel: "models/higgs-engine/VCRUNTIME140_1.dll", expect: 0 },
                Marker { rel: "models/higgs-engine/VCOMP140.DLL", expect: 0 },
            ],
            external_url: None,
        },
        Component {
            id: "ocr",
            name: "OCR-модели (PP-OCR ONNX)",
            purpose: "Детекция вшитого текста → блюр (идут в комплекте)",
            requirement: Requirement::Recommended,
            delivery: Delivery::Bundled,
            size: 31_726_193,
            files: &[],
            markers: &[
                Marker { rel: "models/ocr/det.onnx", expect: 0 },
                Marker { rel: "models/ocr/cls.onnx", expect: 0 },
                Marker { rel: "models/ocr/rec_cyrillic.onnx", expect: 0 },
                Marker { rel: "models/ocr/rec_cyrillic.dict.txt", expect: 0 },
            ],
            external_url: None,
        },
        Component {
            id: "nvidia-driver",
            name: "Драйвер NVIDIA",
            purpose: "GPU-ускорение (ставится отдельно, не приложением)",
            requirement: Requirement::Required,
            delivery: Delivery::External,
            size: 0,
            files: &[],
            markers: &[],
            external_url: Some(NVIDIA_DRIVER_URL),
        },
    ]
}

// ── Статус одного компонента ────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentStatus {
    pub id: String,
    pub name: String,
    pub purpose: String,
    pub requirement: Requirement,
    pub delivery: Delivery,
    /// Ожидаемый совокупный размер, байт.
    pub size: u64,
    /// Установлен ли (для External — детект драйвера; иначе — все маркеры на месте с верным размером).
    pub installed: bool,
    /// Сколько байт уже на диске (для докачки/UI прогресса частично скачанного).
    pub bytes_on_disk: u64,
    /// Список отсутствующих/битых маркеров (для диагностики).
    pub missing: Vec<String>,
    /// Для External: версия драйвера, если задетектена.
    pub detail: Option<String>,
    /// URL внешней страницы (драйвер).
    pub external_url: Option<String>,
    /// Оценка VRAM при загрузке модели, байт (0 для движков/рантаймов без весов).
    pub vram: u64,
}

/// Оценка VRAM загруженной модели по id (движки/рантаймы = 0). Грубо, для показа в UI.
fn vram_estimate(id: &str) -> u64 {
    let gb = |g: f64| (g * 1024.0 * 1024.0 * 1024.0) as u64;
    match id {
        "higgs" => gb(5.6),
        "higgs-q6_k" => gb(5.1),
        "higgs-q4_k_m" => gb(4.2),
        "gemma" => gb(8.5),
        "gemma-q5_0" => gb(9.9),
        "gemma-q6_k" => gb(11.2),
        "gemma-q8_0" => gb(14.0),
        "parakeet" => gb(1.1),
        "parakeet-fp32" => gb(2.7),
        "sortformer" => gb(0.6),
        "roformer" => gb(0.5),
        "roformer-q5" => gb(0.45),
        "roformer-q4" => gb(0.4),
        _ => 0,
    }
}

/// Существует ли маркер и «целый» ли он (размер совпадает, если expect != 0).
fn marker_ok(repo_root: &Path, m: &Marker) -> bool {
    let p = repo_root.join(m.rel);
    match std::fs::metadata(&p) {
        Ok(meta) if meta.is_file() => m.expect == 0 || meta.len() == m.expect,
        _ => false,
    }
}

fn marker_bytes(repo_root: &Path, m: &Marker) -> u64 {
    std::fs::metadata(repo_root.join(m.rel)).map(|x| x.len()).unwrap_or(0)
}

/// ffmpeg доступен в системном PATH? (Command::new("ffmpeg") найдёт его при рендере.)
fn ffmpeg_on_path() -> bool {
    let name = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(name).is_file()))
        .unwrap_or(false)
}

/// Статус компонента на диске.
pub fn component_status(repo_root: &Path, c: &Component) -> ComponentStatus {
    let (installed, detail) = if c.delivery == Delivery::External {
        // Драйвер: детект по загрузке nvcuda.dll (часть драйвера). Версию не тянем (без NVML-зависимости).
        (detect_driver(), None)
    } else if c.id == "ffmpeg" {
        // ffmpeg дублирует пайплайн через PATH: если он уже в системе (Command::new("ffmpeg") найдёт) —
        // считаем установленным и НЕ навязываем закачку. Иначе — по маркеру в tools/ffmpeg.
        let by_marker = c.markers.iter().all(|m| marker_ok(repo_root, m));
        if by_marker {
            (true, Some("tools/ffmpeg".to_string()))
        } else if ffmpeg_on_path() {
            (true, Some("PATH".to_string()))
        } else {
            (false, None)
        }
    } else {
        let all = c.markers.iter().all(|m| marker_ok(repo_root, m));
        (all, None)
    };
    let missing: Vec<String> = c
        .markers
        .iter()
        .filter(|m| !marker_ok(repo_root, m))
        .map(|m| m.rel.to_string())
        .collect();
    let bytes_on_disk: u64 = c.markers.iter().map(|m| marker_bytes(repo_root, m)).sum();
    ComponentStatus {
        id: c.id.to_string(),
        name: c.name.to_string(),
        purpose: c.purpose.to_string(),
        requirement: c.requirement,
        delivery: c.delivery,
        size: c.size,
        installed,
        bytes_on_disk,
        missing,
        detail,
        external_url: c.external_url.map(|s| s.to_string()),
        vram: vram_estimate(c.id),
    }
}

// ── Детект драйвера NVIDIA ───────────────────────────────────────────────────

/// Драйвер установлен, если грузится nvcuda.dll (Windows) / libcuda (Linux) — часть драйвера, не Toolkit.
/// Лёгкий детект без NVML-зависимости: пробуем LoadLibrary. На не-Windows — наличие libcuda в загрузке.
#[cfg(windows)]
pub fn detect_driver() -> bool {
    use std::os::windows::ffi::OsStrExt;
    // LoadLibraryW("nvcuda.dll"); успех => драйвер есть. FreeLibrary опускаем (процесс короткоживущий тут).
    let wide: Vec<u16> = std::ffi::OsStr::new("nvcuda.dll")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        let h = LoadLibraryW(wide.as_ptr());
        !h.is_null()
    }
}

#[cfg(windows)]
extern "system" {
    fn LoadLibraryW(lpLibFileName: *const u16) -> *mut std::ffi::c_void;
}

#[cfg(not(windows))]
pub fn detect_driver() -> bool {
    // На Linux драйвер = libcuda.so.1 в загрузчике. Для портатива на Windows это ветка не активна.
    std::path::Path::new("/usr/lib/x86_64-linux-gnu/libcuda.so.1").exists()
        || std::path::Path::new("/usr/lib/libcuda.so.1").exists()
}

// ── Импорт готовых моделей из выбранной папки ────────────────────────────────

/// Рекурсивно собрать карту basename(lower) -> [(path, size)] под dir (лимит глубины/файлов, чтоб не
/// уйти в бесконечность на большом диске).
fn index_dir(dir: &Path, map: &mut std::collections::HashMap<String, Vec<(PathBuf, u64)>>, depth: usize, budget: &mut usize) {
    if depth > 8 || *budget == 0 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        if *budget == 0 {
            return;
        }
        let p = e.path();
        if p.is_dir() {
            index_dir(&p, map, depth + 1, budget);
        } else if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
            let size = e.metadata().map(|m| m.len()).unwrap_or(0);
            map.entry(name.to_lowercase()).or_default().push((p.clone(), size));
            *budget -= 1;
        }
    }
}

/// Импортировать готовые файлы компонентов из src_dir: для каждого маркера, которого нет на месте, ищем
/// в src_dir файл с тем же именем (и размером, если известен) и КОПИРУЕМ на ожидаемый путь. Возвращает
/// список импортированных id.
pub fn import_from_dir(repo_root: &Path, src_dir: &Path, only: Option<&str>) -> Vec<String> {
    let mut map = std::collections::HashMap::new();
    let mut budget = 200_000usize;
    index_dir(src_dir, &mut map, 0, &mut budget);
    let mut imported = Vec::new();
    for c in manifest() {
        if c.delivery != Delivery::Download {
            continue;
        }
        if let Some(id) = only {
            if c.id != id {
                continue;
            }
        }
        let mut any = false;
        for m in c.markers {
            let dest = repo_root.join(m.rel);
            if marker_ok(repo_root, m) {
                continue;
            }
            let base = Path::new(m.rel).file_name().and_then(|s| s.to_str()).map(|s| s.to_lowercase());
            let Some(base) = base else { continue };
            let Some(cands) = map.get(&base) else { continue };
            // предпочитаем точное совпадение размера, иначе первый попавшийся.
            let pick = cands
                .iter()
                .find(|(_, sz)| m.expect != 0 && *sz == m.expect)
                .or_else(|| cands.first());
            if let Some((src, _)) = pick {
                if let Some(parent) = dest.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if std::fs::copy(src, &dest).is_ok() {
                    any = true;
                }
            }
        }
        if any && component_status(repo_root, &c).installed {
            imported.push(c.id.to_string());
        }
    }
    imported
}

// ── Полный статус (для GET /setup/status) ────────────────────────────────────

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupStatus {
    pub components: Vec<ComponentStatus>,
    /// Всё ли обязательное на месте (тогда фронт показывает обычный hero, а не «первый запуск»).
    pub ready: bool,
    /// Совокупный размер того, что ещё надо скачать (обязательное+рекомендованное, что missing и Download).
    pub download_pending: u64,
    /// Драйвер NVIDIA найден.
    pub driver_ok: bool,
    /// build-строки для диагностики (llama-билд и т.п.).
    pub llama_build: String,
}

pub fn setup_status(repo_root: &Path) -> SetupStatus {
    let comps: Vec<ComponentStatus> = manifest()
        .iter()
        .map(|c| component_status(repo_root, c))
        .collect();
    let ready = comps
        .iter()
        .filter(|c| c.requirement == Requirement::Required)
        .all(|c| c.installed);
    let download_pending = comps
        .iter()
        .filter(|c| c.delivery == Delivery::Download && !c.installed)
        .map(|c| c.size.saturating_sub(c.bytes_on_disk))
        .sum();
    let driver_ok = comps.iter().find(|c| c.id == "nvidia-driver").map(|c| c.installed).unwrap_or(false);
    SetupStatus {
        components: comps,
        ready,
        download_pending,
        driver_ok,
        llama_build: GH_LLAMA_BUILD.to_string(),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  ЗАКАЧКА (тело джобы; прогресс -> колбэк, как у analyze/render)
// ═══════════════════════════════════════════════════════════════════════════

/// Колбэк прогресса скачивания: сервер оборачивает его в SSE-событие джобы.
pub type ProgressCb<'a> = dyn Fn(Value) + 'a;

/// Скачать набор компонентов по id (идемпотентно: уже целые файлы пропускаем). Возвращает JSON-результат
/// с итоговым статусом каждого компонента. Тело синхронное (вызывается из spawn_blocking джобы).
pub fn download_components(
    repo_root: &Path,
    ids: &[String],
    cancel: &dyn Fn() -> bool,
    progress: &ProgressCb,
) -> Result<Value, String> {
    let all = manifest();
    let selected: Vec<&Component> = all
        .iter()
        .filter(|c| ids.iter().any(|x| x == c.id) && c.delivery == Delivery::Download)
        .collect();
    if selected.is_empty() {
        return Err("нет скачиваемых компонентов среди выбранных id".to_string());
    }

    // Общий вес для сквозного процента.
    let grand_total: u64 = selected.iter().map(|c| c.size).sum();
    let mut done_bytes: u64 = 0;
    let mut results = Vec::new();

    let tmp_dir = std::env::temp_dir().join("dub-studio-setup");
    let _ = std::fs::create_dir_all(&tmp_dir);

    for c in selected {
        progress(json!({ "msg": format!("Скачиваю: {}", c.name), "stage": "download", "component": c.id }));
        for f in c.files {
            if cancel() {
                return Err("отменено".to_string());
            }
            download_file_spec(repo_root, &tmp_dir, c, f, grand_total, &mut done_bytes, cancel, progress)?;
        }
        // Проверка после закачки.
        let st = component_status(repo_root, c);
        results.push(json!({ "id": c.id, "installed": st.installed, "missing": st.missing }));
        progress(json!({ "msg": format!("Готово: {}", c.name), "stage": "download", "component": c.id, "installed": st.installed }));
    }

    let overall = setup_status(repo_root);
    Ok(json!({ "components": results, "ready": overall.ready }))
}

/// Скачать одну FileSpec с докачкой-резюме и распаковкой по типу.
#[allow(clippy::too_many_arguments)]
fn download_file_spec(
    repo_root: &Path,
    tmp_dir: &Path,
    comp: &Component,
    f: &FileSpec,
    grand_total: u64,
    done_bytes: &mut u64,
    cancel: &dyn Fn() -> bool,
    progress: &ProgressCb,
) -> Result<(), String> {
    let dest = repo_root.join(f.dest_rel);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("создать {}: {e}", parent.display()))?;
    }

    // Прямой файл уже целый? Пропускаем (идемпотентность).
    if f.extract == Extract::None && f.size != 0 {
        if let Ok(meta) = std::fs::metadata(&dest) {
            if meta.len() == f.size {
                *done_bytes += f.size;
                return Ok(());
            }
        }
    }

    // Куда качать: прямой файл — в dest; архив/wheel — во временный файл рядом.
    let is_archive = f.extract != Extract::None;
    let download_target = if is_archive {
        tmp_dir.join(
            Path::new(f.dest_rel)
                .file_name()
                .map(|s| s.to_os_string())
                .unwrap_or_else(|| "archive.tmp".into()),
        )
    } else {
        dest.clone()
    };

    let base = *done_bytes;
    let comp_id = comp.id.to_string();
    let name = comp.name.to_string();
    let emit = |downloaded: u64, total: u64, speed: f64| {
        let pct = if grand_total > 0 {
            ((base + downloaded) as f64 / grand_total as f64) * 100.0
        } else {
            0.0
        };
        progress(json!({
            "stage": "download",
            "component": comp_id,
            "msg": name,
            "downloaded": base + downloaded,
            "total": grand_total,
            "file_total": total,
            "speed_mbps": speed,
            "pct": pct.min(100.0),
        }));
    };

    http_download(f.url, &download_target, cancel, &emit)?;

    // Распаковка/финализация.
    match f.extract {
        Extract::None => {}
        Extract::ZipFlat => {
            let dir = dest.parent().unwrap_or(repo_root);
            extract_zip_flat(&download_target, dir)?;
            let _ = std::fs::remove_file(&download_target);
        }
        Extract::ZipPick => {
            let dir = dest.parent().unwrap_or(repo_root);
            extract_zip_pick(&download_target, dir)?;
            let _ = std::fs::remove_file(&download_target);
        }
        Extract::ZipTree => {
            let dir = dest.parent().unwrap_or(repo_root);
            extract_zip_tree(&download_target, dir)?;
            let _ = std::fs::remove_file(&download_target);
        }
        Extract::WheelDlls => {
            let dir = dest.parent().unwrap_or(repo_root);
            extract_wheel_dlls(&download_target, dir)?;
            let _ = std::fs::remove_file(&download_target);
        }
    }

    // Учёт: для файла с известным размером прибавляем size, иначе — фактический размер закачанного.
    let added = if f.size != 0 {
        f.size
    } else {
        std::fs::metadata(&download_target).map(|m| m.len()).unwrap_or(0)
    };
    *done_bytes = base + added;
    Ok(())
}

// ── HTTP-загрузка (reqwest blocking; докачка через Range) ────────────────────

/// Скачать URL в файл. Поддерживает резюме: если .part уже есть — просим Range с оффсета. Прогресс —
/// через колбэк (downloaded, total, speed_mbps). Отмена — по cancel().
fn http_download(
    url: &str,
    dest: &Path,
    cancel: &dyn Fn() -> bool,
    emit: &dyn Fn(u64, u64, f64),
) -> Result<(), String> {
    use std::io::{Read, Write};

    let part = dest.with_extension(format!(
        "{}part",
        dest.extension().and_then(|e| e.to_str()).map(|e| format!("{e}.")).unwrap_or_default()
    ));
    let resume_from = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);

    let client = reqwest::blocking::Client::builder()
        .timeout(None)
        .connect_timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("http client: {e}"))?;

    let mut req = client.get(url);
    if resume_from > 0 {
        req = req.header("Range", format!("bytes={resume_from}-"));
    }
    let mut resp = req.send().map_err(|e| format!("GET {url}: {e}"))?;
    let status = resp.status();
    if !(status.is_success() || status.as_u16() == 206) {
        return Err(format!("HTTP {} для {url}", status.as_u16()));
    }
    // Полный размер: content-length + возможный оффсет резюма (206 отдаёт остаток).
    let remaining = resp.content_length().unwrap_or(0);
    let effective_resume = if status.as_u16() == 206 { resume_from } else { 0 };
    let total = remaining + effective_resume;

    // Если сервер проигнорировал Range (200 вместо 206) — начинаем part заново.
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(effective_resume > 0)
        .truncate(effective_resume == 0)
        .open(&part)
        .map_err(|e| format!("открыть {}: {e}", part.display()))?;

    let mut downloaded = effective_resume;
    let mut buf = [0u8; 262_144];
    let start = std::time::Instant::now();
    let mut last_emit = std::time::Instant::now();
    loop {
        if cancel() {
            return Err("отменено".to_string());
        }
        let n = resp.read(&mut buf).map_err(|e| format!("чтение тела: {e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| format!("запись: {e}"))?;
        downloaded += n as u64;
        if last_emit.elapsed().as_millis() > 250 {
            last_emit = std::time::Instant::now();
            let secs = start.elapsed().as_secs_f64().max(0.001);
            let speed = ((downloaded - effective_resume) as f64 / 1_000_000.0) / secs;
            emit(downloaded, total, speed);
        }
    }
    file.flush().map_err(|e| format!("flush: {e}"))?;
    drop(file);
    std::fs::rename(&part, dest).map_err(|e| format!("финализация {}: {e}", dest.display()))?;
    emit(downloaded, total, 0.0);
    Ok(())
}

// ── Распаковка архивов ───────────────────────────────────────────────────────

/// zip: все файлы плоско (только имя) в dir. Для движков-сайдкаров (exe + DLL в одном уровне).
fn extract_zip_flat(zip_path: &Path, dir: &Path) -> Result<(), String> {
    let file = std::fs::File::open(zip_path).map_err(|e| format!("открыть {}: {e}", zip_path.display()))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("не zip: {e}"))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("создать {}: {e}", dir.display()))?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| format!("запись zip: {e}"))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().replace('\\', "/");
        let leaf = name.rsplit('/').next().unwrap_or(&name).to_string();
        if leaf.is_empty() {
            continue;
        }
        write_entry(&mut entry, &dir.join(&leaf))?;
    }
    Ok(())
}

/// zip: отобрать нужные файлы (onnxruntime.dll, ffmpeg.exe/ffprobe.exe) и положить плоско в dir.
/// onnxruntime-win-x64-1.24.2/lib/onnxruntime.dll -> dir/onnxruntime.dll ; ffmpeg .../bin/*.exe -> dir/*.exe.
fn extract_zip_pick(zip_path: &Path, dir: &Path) -> Result<(), String> {
    let file = std::fs::File::open(zip_path).map_err(|e| format!("открыть {}: {e}", zip_path.display()))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("не zip: {e}"))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("создать {}: {e}", dir.display()))?;
    // Целевые: onnxruntime.dll (+.pdb не нужен), ffmpeg.exe, ffprobe.exe. Берём по имени листа.
    const WANT: &[&str] = &["onnxruntime.dll", "ffmpeg.exe", "ffprobe.exe"];
    let mut got = 0;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| format!("запись zip: {e}"))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().replace('\\', "/");
        let leaf = name.rsplit('/').next().unwrap_or(&name).to_ascii_lowercase();
        if WANT.iter().any(|w| *w == leaf) {
            let out_name = name.rsplit('/').next().unwrap_or(&name).to_string();
            write_entry(&mut entry, &dir.join(&out_name))?;
            got += 1;
        }
    }
    if got == 0 {
        return Err(format!("в архиве {} не найдено нужных файлов", zip_path.display()));
    }
    Ok(())
}

/// zip: распаковать весь архив с сохранением поддерева в dir (onnxruntime-win-x64-*/lib/…). Защита от
/// zip-slip: отбрасываем компоненты `..` и абсолютные пути.
fn extract_zip_tree(zip_path: &Path, dir: &Path) -> Result<(), String> {
    let file = std::fs::File::open(zip_path).map_err(|e| format!("открыть {}: {e}", zip_path.display()))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("не zip: {e}"))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("создать {}: {e}", dir.display()))?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| format!("запись zip: {e}"))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().replace('\\', "/");
        // Санитизация пути: только нормальные компоненты, без `..`/корней.
        let mut rel = PathBuf::new();
        for comp in name.split('/') {
            if comp.is_empty() || comp == "." || comp == ".." {
                continue;
            }
            rel.push(comp);
        }
        if rel.as_os_str().is_empty() {
            continue;
        }
        let out = dir.join(&rel);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("создать {}: {e}", parent.display()))?;
        }
        write_entry(&mut entry, &out)?;
    }
    Ok(())
}

/// wheel (zip): все *.dll плоско в dir (CUDA runtime — cudart/cublas/cublasLt).
fn extract_wheel_dlls(wheel_path: &Path, dir: &Path) -> Result<(), String> {
    let file = std::fs::File::open(wheel_path).map_err(|e| format!("открыть {}: {e}", wheel_path.display()))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("wheel не zip: {e}"))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("создать {}: {e}", dir.display()))?;
    let mut written = 0;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| format!("запись wheel: {e}"))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().replace('\\', "/");
        let leaf = name.rsplit('/').next().unwrap_or(&name).to_string();
        if !leaf.to_ascii_lowercase().ends_with(".dll") {
            continue;
        }
        write_entry(&mut entry, &dir.join(&leaf))?;
        written += 1;
    }
    if written == 0 {
        return Err(format!("в wheel {} нет DLL", wheel_path.display()));
    }
    Ok(())
}

/// Записать элемент архива в файл через .part+rename (атомарно).
fn write_entry(entry: &mut zip::read::ZipFile<impl std::io::Read>, out: &Path) -> Result<(), String> {
    let tmp = out.with_extension(format!(
        "{}part",
        out.extension().and_then(|e| e.to_str()).map(|e| format!("{e}.")).unwrap_or_default()
    ));
    {
        let mut fout = std::fs::File::create(&tmp).map_err(|e| format!("создать {}: {e}", tmp.display()))?;
        std::io::copy(entry, &mut fout).map_err(|e| format!("распаковка {}: {e}", out.display()))?;
    }
    std::fs::rename(&tmp, out).map_err(|e| format!("финализация {}: {e}", out.display()))?;
    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_ids_unique_and_nonempty() {
        let m = manifest();
        assert!(!m.is_empty());
        let mut ids: Vec<&str> = m.iter().map(|c| c.id).collect();
        ids.sort_unstable();
        let n = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), n, "id компонентов должны быть уникальны");
    }

    #[test]
    fn download_components_have_files_and_markers() {
        for c in manifest() {
            match c.delivery {
                Delivery::Download => {
                    assert!(!c.files.is_empty(), "{}: Download без files", c.id);
                    assert!(!c.markers.is_empty(), "{}: Download без markers", c.id);
                    for f in c.files {
                        assert!(f.url.starts_with("https://"), "{}: не https URL", c.id);
                    }
                }
                Delivery::Bundled => {
                    assert!(c.files.is_empty(), "{}: Bundled не качается", c.id);
                    assert!(!c.markers.is_empty(), "{}: Bundled без markers", c.id);
                }
                Delivery::External => {
                    assert!(c.external_url.is_some(), "{}: External без url", c.id);
                }
            }
        }
    }

    #[test]
    fn all_installed_on_this_machine() {
        // Юнит из ТЗ (а): манифест против реального диска. На этой машине всё должно быть installed.
        // repo_root резолвим из DUB_STUDIO_ROOT (задаётся в CI/приёмке), иначе — CARGO_MANIFEST_DIR/../..
        let repo_root = std::env::var("DUB_STUDIO_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("..")
                    .join("..")
            });
        if !repo_root.join("models").is_dir() {
            eprintln!("skip: нет {}/models (не приёмочная машина)", repo_root.display());
            return;
        }
        let st = setup_status(&repo_root);
        let mut broken = Vec::new();
        for c in &st.components {
            // Драйвер зависит от железа; на приёмочной машине RTX 4090 он есть, но в headless CI может не быть.
            if c.delivery == Delivery::External {
                continue;
            }
            if !c.installed {
                broken.push(format!("{} (missing: {:?})", c.id, c.missing));
            }
        }
        assert!(broken.is_empty(), "не установлены компоненты: {broken:?}");
    }
}
