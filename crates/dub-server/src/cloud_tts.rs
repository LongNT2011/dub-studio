//! Облачный TTS через OpenRouter (`POST /api/v1/audio/speech`, createSpeech). Опциональная замена
//! локального Higgs. Отдаёт СЫРЫЕ wav-байты — их пишем ПРЯМО в seg_<i>.wav дубляжного конвейера, без
//! декода в сэмплы и без обратной перекодировки (лишние конвертации = зло). Дальнейший fit_to_slot
//! (ffmpeg) сам читает этот wav и подгоняет темп/частоту под слот, как для локального Higgs.
//!
//! Тело запроса: {model, input, voice, response_format:"wav", speed}. Ответ — сырой байтстрим wav
//! (самоописывающий: частота/каналы в заголовке — ffmpeg-фит не нужно ничего сообщать). НЕ mp3 (лоссы).
//! Голос — пресетный (or_tts_voice / пер-спикерный). Модели: openai/gpt-4o-mini-tts,
//! google/gemini-*-flash-tts, mistralai/voxtral-mini-tts, elevenlabs/eleven-turbo-v2 (id в or_tts_model).

use std::path::Path;

/// Синтез одной реплики через OpenRouter TTS -> СЫРЫЕ wav-байты (пишутся прямо в seg-файл). `voice` —
/// голос спикера (пусто -> дефолт из настроек). Ошибка — человекочитаемая строка.
pub fn synth_wav(models_root: &Path, text: &str, voice: &str) -> Result<Vec<u8>, String> {
    let key = crate::models::openrouter_key(models_root)
        .ok_or("облачный TTS включён, но ключ OpenRouter не задан")?;
    let model = crate::models::openrouter_model(models_root, "tts");
    let v = if voice.trim().is_empty() {
        crate::models::openrouter_tts_voice(models_root)
    } else {
        voice.trim().to_string()
    };

    let http = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .map_err(|e| e.to_string())?;
    let body = serde_json::json!({
        "model": model,
        "input": text,
        "voice": v,
        "response_format": "wav", // без потерь + самоописывающий; пишем байты как есть, без переконвертаций
    });
    let resp = http
        .post("https://openrouter.ai/api/v1/audio/speech")
        .bearer_auth(&key)
        .header("HTTP-Referer", "https://github.com/timoncool/dub-studio")
        .header("X-Title", "Dub Studio")
        .json(&body)
        .send()
        .map_err(|e| format!("OpenRouter TTS запрос: {e}"))?;
    let status = resp.status();
    let bytes = resp.bytes().map_err(|e| format!("OpenRouter TTS чтение: {e}"))?;
    if !status.is_success() {
        let msg = String::from_utf8_lossy(&bytes);
        return Err(format!(
            "OpenRouter TTS {status}: {}",
            msg.trim().chars().take(300).collect::<String>()
        ));
    }
    if bytes.len() < 44 {
        return Err(format!("OpenRouter TTS: слишком короткий ответ ({} байт)", bytes.len()));
    }
    Ok(bytes.to_vec())
}
