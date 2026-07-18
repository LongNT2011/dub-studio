//! casting.json — персистентная база персонажей на сериал: имя, пол, speech_note, dub_voice,
//! face_embedding (медоид кластера), voice_embedding (из инфры #83 speaker_global), кадр-аватарка.
//! Матчинг эпизодов: косинус face_embedding+voice_embedding к сохранённым (порог ~0.5).

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Версия схемы casting.json (bump при несовместимом изменении формата).
pub const CASTING_VERSION: u32 = 1;

/// Один персонаж кастинг-базы.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Character {
    /// Стабильный id персонажа (сохраняется между эпизодами при матче).
    pub id: String,
    /// Человекочитаемое имя (правится юзером).
    #[serde(default)]
    pub name: String,
    /// Пол ("male"|"female"|"" неизвестно) — по F0 голоса (f0.rs), для подсказки голоса.
    #[serde(default)]
    pub gender: String,
    /// Заметка о речи персонажа -> подмешивается в промпт Геммы (translate_style, см. link в analyze).
    #[serde(default)]
    pub speech_note: String,
    /// Голос дубляжа: имя из voices/ или "clone" (клонирование голоса спикера).
    #[serde(default)]
    pub dub_voice: String,
    /// id спикеров диаризации, привязанных к персонажу в ЭТОМ эпизоде (обычно один).
    #[serde(default)]
    pub speaker_ids: Vec<String>,
    /// Медоидный эмбеддинг лица (512-d, L2). Пусто = персонаж чисто голосовой (лица нет).
    #[serde(default)]
    pub face_embedding: Vec<f32>,
    /// Эмбеддинг голоса (из speaker_global #83). Пусто = движок эмбеддера голоса недоступен.
    #[serde(default)]
    pub voice_embedding: Vec<f32>,
    /// Путь к кадру-аватарке (относительно work_dir проекта, напр. "casting/char_0.png").
    #[serde(default)]
    pub sample_frame: String,
}

impl Default for Character {
    fn default() -> Self {
        Character {
            id: String::new(),
            name: String::new(),
            gender: String::new(),
            speech_note: String::new(),
            dub_voice: "clone".to_string(),
            speaker_ids: Vec::new(),
            face_embedding: Vec::new(),
            voice_embedding: Vec::new(),
            sample_frame: String::new(),
        }
    }
}

/// Кастинг-база: список персонажей + версия схемы.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Casting {
    pub version: u32,
    /// Тип контента: "real" (лицо=512-d LVFace, косинус) | "anime" (лицо=768-d CCIP, L2). Пространства
    /// эмбеддингов РАЗНЫЕ — кросс-эпизод матчить ТОЛЬКО одинаковый content_type. Пусто = "real" (старые файлы).
    #[serde(default)]
    pub content_type: String,
    #[serde(default)]
    pub characters: Vec<Character>,
}

impl Default for Casting {
    fn default() -> Self {
        Casting { version: CASTING_VERSION, content_type: String::new(), characters: Vec::new() }
    }
}

/// Записать casting.json атомарно (tmp + rename).
pub fn save_casting(path: &Path, casting: &Casting) -> Result<(), String> {
    let json = serde_json::to_string_pretty(casting).map_err(|e| format!("сериализация: {e}"))?;
    let tmp = path.with_extension("json.tmp");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
    }
    std::fs::write(&tmp, json.as_bytes()).map_err(|e| format!("запись tmp: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

/// Прочитать casting.json. Отсутствует/битый -> None (не ошибка: базы просто ещё нет).
pub fn load_casting(path: &Path) -> Option<Casting> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Комбинированный косинус двух персонажей по лицу+голосу. Если у обоих есть face — берём его; добавляем
/// voice, если у обоих есть. Возвращает средневзвешенное доступных модальностей (лицо весит 0.6, голос
/// 0.4 — лицо надёжнее для персонажа). Нет общих модальностей -> -1 (не матчим).
///
/// ВАЖНО (текущее ограничение, #115): voice_embedding пока всегда пуст (эмбеддер голоса = NullEmbedder,
/// см. casting.rs). Поэтому cross-episode фактически работает по ЛИЦУ. Персонаж без лица И без voice-
/// эмбеддинга даёт -1 (ниже любого порога в match_cross_episode) и между эпизодами не матчится — это
/// корректное поведение, не ошибка. Полноценный voice-эмбеддер включит голосовую ветку автоматически.
pub fn character_similarity(a: &Character, b: &Character) -> f32 {
    let face = both_cos(&a.face_embedding, &b.face_embedding);
    let voice = both_cos(&a.voice_embedding, &b.voice_embedding);
    match (face, voice) {
        (Some(f), Some(v)) => 0.6 * f + 0.4 * v,
        (Some(f), None) => f,
        (None, Some(v)) => v,
        (None, None) => -1.0,
    }
}

/// Косинус, только если ОБА непусты и одной длины; иначе None.
fn both_cos(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.is_empty() || b.is_empty() || a.len() != b.len() {
        return None;
    }
    Some(dub_asr::cosine(a, b))
}

/// Сматчить персонажей нового эпизода с сохранённой базой прошлого. Для каждого нового персонажа ищем
/// самого похожего в prev с similarity >= threshold; при матче ПЕРЕНОСИМ id/name/gender/speech_note/
/// dub_voice из prev (сохраняем правки юзера), оставляя эпизод-специфичные speaker_ids/embeddings/аватар
/// нового. Взаимно-однозначно (один prev-персонаж не матчится дважды — жадно по убыванию similarity).
/// Возвращает НОВЫЙ список персонажей (порядок как во входном `current`).
pub fn match_cross_episode(
    current: &[Character],
    prev: &Casting,
    threshold: f32,
) -> Vec<Character> {
    let mut out = current.to_vec();
    // Все пары (similarity, cur_idx, prev_idx) >= threshold, детерминированная сортировка.
    let mut pairs: Vec<(f32, usize, usize)> = Vec::new();
    for (ci, c) in current.iter().enumerate() {
        for (pi, p) in prev.characters.iter().enumerate() {
            let sim = character_similarity(c, p);
            if sim >= threshold {
                pairs.push((sim, ci, pi));
            }
        }
    }
    pairs.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.cmp(&b.1))
            .then(a.2.cmp(&b.2))
    });
    let mut used_cur = vec![false; current.len()];
    let mut used_prev = vec![false; prev.characters.len()];
    for (_, ci, pi) in pairs {
        if used_cur[ci] || used_prev[pi] {
            continue;
        }
        used_cur[ci] = true;
        used_prev[pi] = true;
        let p = &prev.characters[pi];
        // Переносим устойчивую идентичность + правки юзера; эпизод-данные (embeddings/speaker_ids/аватар)
        // оставляем от нового эпизода.
        out[ci].id = p.id.clone();
        out[ci].name = p.name.clone();
        if !p.gender.is_empty() {
            out[ci].gender = p.gender.clone();
        }
        out[ci].speech_note = p.speech_note.clone();
        out[ci].dub_voice = p.dub_voice.clone();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn char_with(id: &str, face: Vec<f32>, voice: Vec<f32>) -> Character {
        Character {
            id: id.to_string(),
            face_embedding: crate::l2_normalize(face),
            voice_embedding: crate::l2_normalize(voice),
            ..Default::default()
        }
    }

    #[test]
    fn save_load_roundtrip() {
        let dir = std::env::temp_dir().join("dubfaces_cast_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("casting.json");
        let mut casting = Casting::default();
        casting.characters.push(char_with("c0", vec![1.0, 0.0], vec![0.0, 1.0]));
        casting.characters[0].name = "Босс".to_string();
        save_casting(&path, &casting).unwrap();
        let loaded = load_casting(&path).unwrap();
        assert_eq!(loaded.version, CASTING_VERSION);
        assert_eq!(loaded.characters.len(), 1);
        assert_eq!(loaded.characters[0].name, "Босс");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_is_none() {
        assert!(load_casting(Path::new("no_such_casting_file.json")).is_none());
    }

    #[test]
    fn cross_episode_transfers_identity() {
        // Новый персонаж с тем же лицом, что prev "hero" -> переносит id/name/voice.
        let mut prev = Casting::default();
        let mut hero = char_with("hero", vec![1.0, 0.0, 0.0], vec![0.0, 1.0, 0.0]);
        hero.name = "Герой".to_string();
        hero.dub_voice = "Мужской1".to_string();
        hero.speech_note = "грубый".to_string();
        prev.characters.push(hero);

        let mut cur = char_with("tmp0", vec![0.98, 0.05, 0.0], vec![0.02, 0.99, 0.0]);
        cur.speaker_ids = vec!["1".to_string()];
        let matched = match_cross_episode(&[cur], &prev, 0.5);
        assert_eq!(matched[0].id, "hero", "id перенесён");
        assert_eq!(matched[0].name, "Герой");
        assert_eq!(matched[0].dub_voice, "Мужской1");
        assert_eq!(matched[0].speech_note, "грубый");
        assert_eq!(matched[0].speaker_ids, vec!["1".to_string()], "speaker_ids эпизода сохранены");
    }

    #[test]
    fn cross_episode_no_match_keeps_new() {
        // Новый персонаж не похож ни на кого -> id/name остаются свои.
        let mut prev = Casting::default();
        prev.characters.push(char_with("hero", vec![1.0, 0.0], vec![]));
        let cur = char_with("tmp0", vec![0.0, 1.0], vec![]);
        let matched = match_cross_episode(&[cur], &prev, 0.5);
        assert_eq!(matched[0].id, "tmp0", "нет матча -> новый id");
    }

    #[test]
    fn similarity_no_common_modality_is_negative() {
        let a = Character { face_embedding: vec![1.0, 0.0], ..Default::default() };
        let b = Character { voice_embedding: vec![1.0, 0.0], ..Default::default() };
        assert_eq!(character_similarity(&a, &b), -1.0);
    }
}
