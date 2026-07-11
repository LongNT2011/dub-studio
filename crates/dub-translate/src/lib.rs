//! dub-translate — стадия перевода + vision над dub-llm (Gemma через llama-server).
//!
//! Порт dubengine/translate.py (плоский MT + rewrite) и ctx_translate.py (единый Gemma-проход: vision
//! layout/scene + audio-контекст + перевод всего транскрипта с контекстом). Промпты и параметры сэмплинга
//! перенесены ДОСЛОВНО — они выверены на тест-сете продукта. Дефолт-модель Gemma-4 12B QAT + mmproj.

mod ctx;
mod seg;
mod translate;
mod vision;

pub use ctx::{run as ctx_run, CtxConfig, CtxResult};
pub use seg::Seg;
pub use translate::{rewrite as flat_rewrite, run as flat_run};
pub use vision::{analyze_layout, is_counter, scene_context, Layout, FONTS};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum TranslateError {
    #[error("llm: {0}")]
    Llm(#[from] dub_llm::LlmError),
    #[error("frame extract: {0}")]
    Frame(String),
    #[error("audio ctx: {0}")]
    Audio(String),
    #[error("MT returned empty for all {0} segments")]
    Empty(usize),
}
