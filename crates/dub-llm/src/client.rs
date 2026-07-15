//! OpenAI-совместимый чат-клиент к llama-server (/v1/chat/completions). Порт вызовов llama_cpp
//! create_chat_completion из translate.py / ctx_translate.py: system+user сообщения, sampling-параметры
//! (temperature/top_p/top_k/repeat_penalty), max_tokens; content как строка ИЛИ массив частей
//! (text / image_url data:base64 / input_audio) для мультимодальности через mmproj.
//!
//! <think>...</think> вырезаем на стороне вызывающего (как в питоне) — тут отдаём сырой content.

use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};

use crate::LlmError;

/// Часть сообщения content. Соответствует list-форме content в питоне:
///   {"type":"text","text":...} | {"type":"image_url","image_url":{"url":"data:image/png;base64,..."}}
///   | {"type":"input_audio","input_audio":{"data":<base64>,"format":"wav"}}
#[derive(Clone)]
pub enum Part {
    Text(String),
    /// PNG-кадр как base64 (без префикса) — обёрнём в data:image/png;base64,.
    ImagePngB64(String),
    /// WAV-аудио как base64 (без префикса) + формат ("wav").
    AudioB64 { data: String, format: String },
}

impl Part {
    fn to_value(&self) -> Value {
        match self {
            Part::Text(t) => json!({"type":"text","text": t}),
            Part::ImagePngB64(b64) => json!({
                "type":"image_url",
                "image_url": {"url": format!("data:image/png;base64,{b64}")}
            }),
            Part::AudioB64 { data, format } => json!({
                "type":"input_audio",
                "input_audio": {"data": data, "format": format}
            }),
        }
    }
}

/// Роль + content одного сообщения. content — либо строка, либо массив частей (мультимодальность).
#[derive(Clone)]
pub struct Message {
    pub role: String,
    pub parts_text: Option<String>, // строковый content (быстрый путь для чисто текстовых вызовов)
    pub parts: Option<Vec<Part>>,   // массив частей (image/audio/text)
}

impl Message {
    /// Текстовое сообщение с заданной ролью (общий конструктор system/user_text).
    fn text_msg(role: &str, text: impl Into<String>) -> Self {
        Message {
            role: role.into(),
            parts_text: Some(text.into()),
            parts: None,
        }
    }
    pub fn system(text: impl Into<String>) -> Self {
        Self::text_msg("system", text)
    }
    pub fn user_text(text: impl Into<String>) -> Self {
        Self::text_msg("user", text)
    }
    pub fn user_parts(parts: Vec<Part>) -> Self {
        Message {
            role: "user".into(),
            parts_text: None,
            parts: Some(parts),
        }
    }

    fn to_value(&self) -> Value {
        let content = if let Some(t) = &self.parts_text {
            Value::String(t.clone())
        } else if let Some(ps) = &self.parts {
            Value::Array(ps.iter().map(|p| p.to_value()).collect())
        } else {
            Value::String(String::new())
        };
        json!({"role": self.role, "content": content})
    }
}

/// Sampling-параметры одного вызова. Дефолты нейтральны; вызывающий выставляет ровно то, что в питоне.
#[derive(Clone, Serialize)]
pub struct Sampling {
    pub temperature: f32,
    pub top_p: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repeat_penalty: Option<f32>,
    pub max_tokens: u32,
}

impl Sampling {
    pub fn new(temperature: f32, top_p: f32, max_tokens: u32) -> Self {
        Sampling {
            temperature,
            top_p,
            top_k: None,
            repeat_penalty: None,
            max_tokens,
        }
    }
    pub fn top_k(mut self, k: i32) -> Self {
        self.top_k = Some(k);
        self
    }
    pub fn repeat_penalty(mut self, r: f32) -> Self {
        self.repeat_penalty = Some(r);
        self
    }
}

/// Клиент чата к одному llama-server. Держит base_url + blocking reqwest client с большим таймаутом
/// (генерация целого транскрипта может идти десятки секунд).
pub struct ChatClient {
    base_url: String,
    http: reqwest::blocking::Client,
    retries: u32,
}

impl ChatClient {
    pub fn new(base_url: impl Into<String>) -> Result<Self, LlmError> {
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(600)) // целый транскрипт/vision-кадр может генериться долго
            .build()
            .map_err(|e| LlmError::Http(e.to_string()))?;
        Ok(ChatClient {
            base_url: base_url.into(),
            http,
            retries: 2,
        })
    }

    pub fn with_retries(mut self, n: u32) -> Self {
        self.retries = n;
        self
    }

    /// Один вызов /v1/chat/completions -> текст ассистента (сырой, без стрипа <think>). Ретраит на
    /// сетевых/5xx-ошибках (сервер мог ещё догружать слот). Пустой ответ — не ошибка (вернём "").
    pub fn chat(&self, messages: &[Message], s: &Sampling) -> Result<String, LlmError> {
        let url = format!("{}/v1/chat/completions", self.base_url);
        // enable_thinking=false — как Gemma4ChatHandler(enable_thinking=False) в питоне: без него Gemma-4
        // сжигает весь max_tokens на reasoning_content, а content приходит пустым (проверено на стенде).
        // top_k/repeat_penalty ВСТАВЛЯЕМ только когда заданы: llama-server отвергает явный null (400).
        let mut body = serde_json::Map::new();
        body.insert(
            "messages".into(),
            Value::Array(messages.iter().map(|m| m.to_value()).collect()),
        );
        body.insert("temperature".into(), json!(s.temperature));
        body.insert("top_p".into(), json!(s.top_p));
        if let Some(k) = s.top_k {
            body.insert("top_k".into(), json!(k));
        }
        if let Some(rp) = s.repeat_penalty {
            body.insert("repeat_penalty".into(), json!(rp));
        }
        body.insert("max_tokens".into(), json!(s.max_tokens));
        body.insert("stream".into(), json!(false));
        body.insert(
            "chat_template_kwargs".into(),
            json!({"enable_thinking": false}),
        );
        let body = Value::Object(body);

        let mut last_err = String::new();
        for attempt in 0..=self.retries {
            match self.http.post(&url).json(&body).send() {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        let v: Value = resp
                            .json()
                            .map_err(|e| LlmError::Http(format!("parse json: {e}")))?;
                        let txt = v
                            .pointer("/choices/0/message/content")
                            .and_then(|c| c.as_str())
                            .unwrap_or("")
                            .to_string();
                        return Ok(txt);
                    }
                    // 4xx (кроме 429) — не ретраим, это наша ошибка запроса.
                    let text = resp.text().unwrap_or_default();
                    if status.as_u16() != 429 && status.as_u16() < 500 {
                        return Err(LlmError::Api(format!("{status}: {text}")));
                    }
                    last_err = format!("{status}: {text}");
                }
                Err(e) => last_err = e.to_string(),
            }
            if attempt < self.retries {
                std::thread::sleep(Duration::from_millis(500 * (attempt as u64 + 1)));
            }
        }
        Err(LlmError::Api(format!(
            "chat failed after {} retries: {last_err}",
            self.retries + 1
        )))
    }
}
