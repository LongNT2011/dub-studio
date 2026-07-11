//! dub-llm — сайдкар llama.cpp (llama-server) + OpenAI-совместимый чат-клиент с vision.
//!
//! Штатный способ работы llama.cpp с мультимодальностью: поднять llama-server (свободный localhost-порт),
//! слать /v1/chat/completions с картинками (base64 в content), выключить на Drop. Порт того, как
//! translate.py / ctx_translate.py грузят Gemma один раз и делают серию сфокусированных вызовов.
//!
//! Дефолты моделей (Gemma-4 12B QAT q4_0 + mmproj) — из dub-core EngineOpts; их сюда передаёт сервер.

mod client;
mod server;

pub use client::{ChatClient, Message, Part, Sampling};
pub use server::{resolve_llama_bin, LlamaServer, ServerOpts};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum LlmError {
    #[error("llama-server spawn: {0}")]
    Spawn(String),
    #[error("http: {0}")]
    Http(String),
    #[error("api: {0}")]
    Api(String),
}

/// Обрезать блок рассуждений <think>...</think> — как re.sub(r"<think>.*?</think>", "", ...) в питоне.
/// Нужно на КАЖДОМ ответе Gemma (translate.py делает это везде).
pub fn strip_think(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let lower = s.to_ascii_lowercase();
    let mut i = 0;
    while i < s.len() {
        if lower[i..].starts_with("<think>") {
            if let Some(end) = lower[i..].find("</think>") {
                i += end + "</think>".len();
                continue;
            } else {
                // незакрытый <think> — отбрасываем остаток (как нежадный .*? не сматчил бы, но безопаснее убрать)
                break;
            }
        }
        // копируем один UTF-8 символ целиком
        let ch_len = utf8_len(bytes[i]);
        out.push_str(&s[i..i + ch_len]);
        i += ch_len;
    }
    out.trim().to_string()
}

fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else if b >> 3 == 0b11110 {
        4
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_think_removes_block() {
        assert_eq!(strip_think("<think>reason</think>hello"), "hello");
        assert_eq!(strip_think("a<think>x</think>b"), "ab");
        assert_eq!(strip_think("plain"), "plain");
    }

    #[test]
    fn strip_think_keeps_unicode() {
        assert_eq!(strip_think("<think>y</think>привет"), "привет");
        assert_eq!(strip_think("こんにちは"), "こんにちは");
    }
}
