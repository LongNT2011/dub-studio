//! EngineOpts — настройки модели/устройства/квантизации/путей (ЧЕМ дублируем).
//! Зеркало dubengine/opts.py. Per-job творческие выборы (mode/voice/язык) живут в Project, не здесь.
//! models-root переопределяется через env DUBENGINE_MODELS_ROOT (портативность).

use std::path::PathBuf;

/// Корень моделей: env DUBENGINE_MODELS_ROOT, иначе ./models рядом с приложением.
fn models_root() -> PathBuf {
    if let Ok(v) = std::env::var("DUBENGINE_MODELS_ROOT") {
        return PathBuf::from(v);
    }
    // Портативный дефолт: <app>/models. Точное значение резолвит сервер/shell.
    PathBuf::from("models")
}

#[derive(Clone, Debug)]
pub struct EngineOpts {
    pub device: String,   // torch / ASR device
    pub provider: String, // onnxruntime EP (ASR / separation / text-detect)
    pub num_threads: i32,

    // ASR — Parakeet-TDT-0.6B-v3 через parakeet-rs (в Python: onnx-asr), int8
    pub asr_model: String,
    pub asr_quant: String,
    // separation (sherpa-onnx / audio-separator)
    pub sep_model: String,
    // MT + vision — Gemma GGUF (llama.cpp); mmproj = vision-проектор
    pub mt_model_path: PathBuf,
    pub mmproj_path: PathBuf,
    // TTS — Higgs Audio v3 через audiocpp_engine.dll
    pub tts_model: String,
    pub tts_quant: String,
    pub tts_steps: i32,
    pub tts_cuda_graphs: bool,
    pub tts_triton: bool,
    // диаризация — Sortformer (в Rust через parakeet-rs sortformer, в Python — NeMo subprocess)
    pub sortformer_model: String,
    pub sortformer_python: Option<PathBuf>,
    // voice packs (клон-референсы / голоса пакета)
    pub voice_pack: PathBuf,
    // render knobs
    pub burn_cq: i32,
    pub blur_sigma: i32,
    pub caption_fps: i32,
    pub max_stretch: f32,
}

impl Default for EngineOpts {
    fn default() -> Self {
        let root = models_root();
        let mt = root.join("mt").join("gemma-4-12b-it-qat-q4_0.gguf");
        let mmproj = mt
            .parent()
            .map(|p| p.join("mmproj-gemma-4-12b-it-qat-q4_0.gguf"))
            .unwrap_or_else(|| PathBuf::from("mmproj-gemma-4-12b-it-qat-q4_0.gguf"));

        let voice_pack = std::env::var("SHORTS_DUB_VOICES")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(r"D:\voice pack\examples"));

        let sortformer_python = std::env::var("SHORTS_DUB_SORTFORMER_PY")
            .map(PathBuf::from)
            .ok();

        EngineOpts {
            device: "cuda".to_string(),
            provider: "cuda".to_string(),
            num_threads: 8,
            asr_model: "nemo-parakeet-tdt-0.6b-v3".to_string(),
            asr_quant: "int8".to_string(),
            sep_model: "UVR-MDX-NET-Inst_HQ_3.onnx".to_string(),
            mt_model_path: mt,
            mmproj_path: mmproj,
            tts_model: "higgs-audio-v3-tts-4b".to_string(),
            tts_quant: "q8_0".to_string(),
            tts_steps: 10,
            tts_cuda_graphs: true,
            tts_triton: true,
            sortformer_model: "diar_streaming_sortformer_4spk-v2".to_string(),
            sortformer_python,
            voice_pack,
            burn_cq: 24,
            blur_sigma: 60,
            caption_fps: 4,
            max_stretch: 2.0,
        }
    }
}
