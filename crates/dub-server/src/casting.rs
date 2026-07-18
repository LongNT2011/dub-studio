//! Стадия кастинга персонажей (#115) для analyze. Порядок: sample_frames (ffmpeg ~1.5fps) -> SCRFD
//! detect -> LVFace embed -> кластеризация в персонажей -> связка лицо↔голос (со-встречаемость с
//! репликами спикеров) -> аватарки -> casting.json. Плюс cross-episode матчинг с прошлым эпизодом.
//!
//! ГЕЙТ: вызывается ТОЛЬКО при proj.casting_enabled (иначе НИ ОДНОГО кадра не сканируется — ноль
//! оверхеда, как OCR по галочке). Fail-safe: сбой (нет весов / SCRFD упал) логируется в SSE и НЕ валит
//! analyze — кастинг не блокер, транскрипт/перевод уже валидны.

use std::collections::HashMap;
use std::path::Path;

use dub_core::Project;
use dub_faces::{
    build_casting, crop_sharpness, frontality, load_casting, match_cross_episode, save_casting,
    FaceAccumulator, FacesModels, FrameDisposition, FrameFace, LvFace, Scrfd, SpeakerTurn,
};

use crate::analyze::{AnalyzePaths, Progress};

// Реальные стадии кастинга — совпадают с ANALYZE_STEPS.casting фронта (App.tsx), чтобы степпер
// подсвечивал шаг «Кастинг» и двигался: cast_detect (кадры+детект лиц) -> cast_embed (эмбеддинги+
// кластеризация) -> cast_speaker (связка лицо↔голос+аватарки+casting.json).
fn emit(progress: &Progress, stage: &str, msg: &str) {
    progress(serde_json::json!({ "stage": stage, "msg": msg }));
}

// Value импорт не нужен — все json! литералы через serde_json::json!.

/// Fps семплинга кадров кастинга. Env DUB_FACES_FPS, иначе дефолт dub-faces (1.5).
fn casting_fps() -> f64 {
    std::env::var("DUB_FACES_FPS")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(dub_faces::DEFAULT_FPS)
}

/// Прогнать стадию кастинга. proj уже собран транскрипт-стадией (segments + speaker). Пишет casting.json
/// в work_dir и аватарки в work_dir/casting/. Ничего не возвращает — результат в файле (эндпоинт отдаёт).
pub fn stage(paths: &AnalyzePaths, proj: &Project, progress: &Progress) {
    // Вторая линия защиты гейта нулевого оверхеда (#115): вызывающий проверяет casting_enabled, но
    // страхуемся здесь — при выключенном кастинге НИ ОДНОГО кадра не сканируем.
    if !proj.casting_enabled {
        return;
    }
    let models = FacesModels::resolve(&paths.models_root);
    if !models.available() {
        emit(
            progress,
            "cast_detect",
            &format!(
                "кастинг пропущен: нет весов (SCRFD/LVFace) в {}",
                paths.models_root.join("faces").display()
            ),
        );
        return;
    }
    if proj.segments.is_empty() {
        emit(progress, "cast_detect", "кастинг пропущен: нет речевых сегментов");
        return;
    }

    // 1-2) ПОТОКОВЫЙ конвейер (#115 перф/OOM): кадры декодируются по одному (stream_frames), сразу
    // детект+эмбеддинг, PNG удаляется тут же (Keep только у кадров с принятым лицом — кандидаты в
    // аватарки). Раньше все декодированные кадры + все PNG держались в RAM/на диске: на 90мин@1.5fps
    // ~8100 кадров = гигабайты (OOM на 32ГБ). Теперь в памяти — только принятые FrameFace (кап 4000).
    let frames_dir = paths.work_dir.join("casting").join("frames");
    let mut scrfd = match Scrfd::load(&models.scrfd) {
        Ok(s) => s,
        Err(e) => {
            emit(progress, "cast_detect", &format!("SCRFD не загрузился: {e}; кастинг пропущен"));
            return;
        }
    };
    let mut lvface = match LvFace::load(&models.lvface) {
        Ok(s) => s,
        Err(e) => {
            emit(progress, "cast_embed", &format!("LVFace не загрузился: {e}; кастинг пропущен"));
            return;
        }
    };

    emit(progress, "cast_detect", "семплинг кадров + детекция лиц (ffmpeg + SCRFD + LVFace)");
    // Фильтрация+дедуп+кап лиц ДО O(N²)-кластеризации: score/bbox-порог, дедуп почти-идентичных соседних
    // кадров, хард-кап (см. FaceAccumulator).
    let mut acc = FaceAccumulator::new();
    // avatar_src[i] — путь к кадру i-го ПРИНЯТОГО лица (индексы синхронны с faces), для копии аватарки.
    let mut avatar_src: Vec<std::path::PathBuf> = Vec::new();
    let stream = dub_faces::stream_frames(&paths.input, &frames_dir, casting_fps(), |fr| {
        if acc.is_full() {
            // Кап достигнут — этот и следующие кадры не нужны; PNG удаляем (кроме уже помеченных Keep).
            return Ok(FrameDisposition::Delete);
        }
        let detected = match scrfd.detect(&fr.img) {
            Ok(d) => d,
            Err(_) => return Ok(FrameDisposition::Delete), // кадр не удался -> пропускаем, не валим прогон
        };
        let mut keep = false;
        for face in &detected {
            let emb = match lvface.embed_face(&fr.img, face) {
                Ok(e) => e,
                Err(_) => continue,
            };
            let bbox = (face.x1, face.y1, face.x2, face.y2);
            let ff = FrameFace {
                t: fr.t,
                bbox,
                score: face.score,
                sharpness: crop_sharpness(&fr.img, bbox),
                frontality: frontality(face),
                embedding: emb,
            };
            let idx = avatar_src.len();
            if acc.offer(ff, idx) {
                avatar_src.push(fr.path.clone());
                keep = true; // кадр — кандидат в аватарку -> PNG оставляем
            }
        }
        Ok(if keep { FrameDisposition::Keep } else { FrameDisposition::Delete })
    });
    let processed = match stream {
        Ok(n) => n,
        Err(e) => {
            emit(progress, "cast_detect", &format!("не удалось извлечь кадры: {e}; кастинг пропущен"));
            return;
        }
    };
    if processed == 0 {
        emit(progress, "cast_detect", "кадров нет (аудио-файл?); кастинг пропущен");
        return;
    }
    let (faces, _avatar_idx, stats) = acc.finish();
    emit(progress, "cast_detect", &format!("кадров обработано: {processed}"));
    // Лог отбраковки (правило «no silent caps»): сколько лиц отброшено и почему.
    let dropped = stats.dropped_score + stats.dropped_bbox + stats.dropped_dedup + stats.dropped_cap;
    if dropped > 0 {
        emit(
            progress,
            "cast_embed",
            &format!(
                "лиц принято: {} (отброшено {dropped}: score {}, bbox {}, дубли {}, кап {})",
                stats.kept, stats.dropped_score, stats.dropped_bbox, stats.dropped_dedup, stats.dropped_cap
            ),
        );
    }
    if stats.dropped_cap > 0 {
        emit(
            progress,
            "cast_embed",
            &format!(
                "достигнут хард-кап лиц ({}) — дальнейшие кадры не сканировались; поднимите DUB_FACES_MAX при необходимости",
                dub_faces::max_faces_cap()
            ),
        );
    }
    if faces.is_empty() {
        emit(progress, "cast_embed", "лиц не найдено; casting.json не создан");
        return;
    }
    // avatar_src синхронен с faces (offer принимает лица по порядку добавления в avatar_src).
    debug_assert_eq!(avatar_src.len(), faces.len());
    emit(progress, "cast_embed", &format!("лиц детектировано: {}", faces.len()));

    // 3) реплики спикеров + пол (по F0, из voice_slots).
    let turns: Vec<SpeakerTurn> = proj
        .segments
        .iter()
        .map(|s| SpeakerTurn {
            start: s.start,
            end: s.end,
            speaker: s.speaker.clone().unwrap_or_else(|| "0".to_string()),
        })
        .collect();
    let mut speakers: Vec<String> = turns.iter().map(|t| t.speaker.clone()).collect();
    speakers.sort();
    speakers.dedup();

    // Пол спикеров по F0 (переиспользуем f0 на чистом вокале, если он есть). Пустой — без пола.
    let genders = speaker_genders(paths, proj, &speakers);
    // Эмбеддинги голоса (#83 speaker_global) — движок эмбеддера пока заглушка (NullEmbedder), поэтому
    // карта ПУСТА. Следствие: cross-episode матчинг работает ТОЛЬКО для персонажей С ЛИЦОМ (по face-
    // эмбеддингу). Чисто голосовые/закадровые персонажи (лица нет И voice-эмбеддинга нет) имеют
    // similarity=-1 и между эпизодами НЕ матчатся — это ожидаемо, не падаем. Полноценный voice-эмбеддер —
    // отдельная задача; тогда голос добавится в матчинг автоматически (character_similarity уже готов).
    let voice_embeddings: HashMap<String, Vec<f32>> = HashMap::new();

    // 4) кластеризация + связка лицо↔голос -> Casting (стадия cast_speaker).
    emit(progress, "cast_speaker", "связка лицо↔голос");
    let cos = dub_faces::cluster_cos_threshold();
    let (mut casting, clusters) =
        build_casting(&faces, &turns, &speakers, &voice_embeddings, &genders, cos);
    emit(progress, "cast_speaker", &format!("персонажей (кластеров лиц): {}", casting.characters.len()));

    // 5) аватарки: сохранить кадр-аватарку каждого кластера в casting/char_<i>.png.
    let cast_dir = paths.work_dir.join("casting");
    let _ = std::fs::create_dir_all(&cast_dir);
    for (ci, cl) in clusters.iter().enumerate() {
        // .get вместо avatar_src[cl.avatar]: при рассинхроне индексов не паникуем, а логируем и пропускаем.
        let Some(src) = avatar_src.get(cl.avatar) else {
            emit(progress, "cast_speaker", &format!("аватарка char_{ci} пропущена: индекс {} вне avatar_src", cl.avatar));
            continue;
        };
        let bbox = faces[cl.avatar].bbox;
        let (bx1, by1, bx2, by2) = bbox;
        // Диагностика (правило «смотреть свой вывод»): сколько кадров у персонажа, размер лица, score.
        emit(
            progress,
            "cast_speaker",
            &format!(
                "char_{ci}: кадров {}, лицо {}×{}px, score {:.2}",
                cl.members.len(),
                (bx2 - bx1) as i32,
                (by2 - by1) as i32,
                faces[cl.avatar].score
            ),
        );
        let out = cast_dir.join(format!("char_{ci}.png"));
        // Кроп ЛИЦА по bbox (+поля), а не копия целого кадра — иначе аватар = весь кадр (руки/фон), #115-баг.
        match dub_faces::save_face_crop(src, bbox, 0.35, &out) {
            Ok(()) => {
                if let Some(ch) = casting.characters.get_mut(ci) {
                    ch.sample_frame = format!("casting/char_{ci}.png");
                }
            }
            Err(e) => emit(progress, "cast_speaker", &format!("аватарка char_{ci} не сохранена: {e}")),
        }
    }

    // 6) cross-episode: если рядом есть casting.json прошлого эпизода (env DUB_FACES_PREV_CASTING или
    //    соседний …/prev_casting.json), матчим и переносим имена/голоса/speech_note.
    if let Some(prev) = load_prev_casting(paths) {
        let mt = dub_faces::match_cos_threshold();
        casting.characters = match_cross_episode(&casting.characters, &prev, mt);
        let carried = casting.characters.iter().filter(|c| !c.name.is_empty()).count();
        emit(progress, "cast_speaker", &format!("cross-episode: перенесено имён/голосов: {carried}"));
    }

    // 7) записать casting.json.
    let path = paths.work_dir.join("casting.json");
    match save_casting(&path, &casting) {
        Ok(()) => emit(progress, "cast_speaker", &format!("casting.json готов: {} персонаж(ей)", casting.characters.len())),
        Err(e) => emit(progress, "cast_speaker", &format!("не удалось записать casting.json: {e}")),
    }
}

// Границы серой зоны/валидного диапазона F0 для КАСТИНГА. Пол в UI показывается фактом, поэтому здесь
// строже voice_slots: пороги пола f0.rs (male<155, female>165); берём с запасом [150..170] как «серую
// зону» неопределённости, вне [F0_MIN..F0_MAX]=[60..400] замер считаем шумом. Числа синхронны с f0.rs
// (MALE_HZ/FEMALE_HZ) и voice_slots (F0_MIN/F0_MAX) — при их изменении обновить здесь.
const GENDER_GRAY_LO: f64 = 150.0;
const GENDER_GRAY_HI: f64 = 170.0;
const GENDER_F0_MIN: f64 = 60.0;
const GENDER_F0_MAX: f64 = 400.0;

/// Ярлык пола по F0 для UI-карточки: "male"/"female" ТОЛЬКО при уверенном замере. Серая зона у порогов
/// или F0 вне валидного диапазона -> None (пол неизвестен, не гадаем).
fn gender_label(f0: f64) -> Option<&'static str> {
    if !(GENDER_F0_MIN..=GENDER_F0_MAX).contains(&f0) {
        return None; // вне человеческого диапазона -> шумный замер
    }
    if (GENDER_GRAY_LO..=GENDER_GRAY_HI).contains(&f0) {
        return None; // серая зона у порогов male/female -> неизвестно
    }
    match crate::f0::gender_of(f0) {
        crate::f0::Gender::Male => Some("male"),
        crate::f0::Gender::Female => Some("female"),
    }
}

/// Пол каждого спикера по медиане F0 на чистом вокале (vocals16_clean.wav / vocals16.wav). Нет вокала ->
/// пустая карта (без пола). Переиспользует crate::f0 (тот же путь, что voice_slots #114).
fn speaker_genders(paths: &AnalyzePaths, proj: &Project, speakers: &[String]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let vocals = {
        let clean = paths.work_dir.join("vocals16_clean.wav");
        let raw = paths.work_dir.join("vocals16.wav");
        if clean.is_file() {
            clean
        } else if raw.is_file() {
            raw
        } else {
            return out;
        }
    };
    for spk in speakers {
        // длиннейшая реплика спикера — устойчивый замер F0.
        let cand = proj
            .segments
            .iter()
            .filter(|s| s.speaker.as_deref().unwrap_or("0") == spk)
            .max_by(|a, b| (a.end - a.start).partial_cmp(&(b.end - b.start)).unwrap_or(std::cmp::Ordering::Equal));
        let Some(seg) = cand else { continue };
        let tmp = paths.work_dir.join(format!("casting_f0_{}.wav", sanitize(spk)));
        let end = seg.end.min(seg.start + 30.0).max(seg.start + 0.05);
        if crate::media::trim(&vocals, &tmp, seg.start, end, 16_000).is_err() {
            continue;
        }
        if let Ok((mono, sr)) = crate::wavio::read_mono_f32(&tmp) {
            if let Some(f) = crate::f0::median_f0(&mono, sr) {
                // Пол ставим ФАКТОМ (UI-карточка), поэтому в неуверенных случаях лучше "" (неизвестно),
                // чем гадать: серая зона у порогов или F0 вне валидного диапазона -> gender="".
                if let Some(g) = gender_label(f) {
                    out.insert(spk.clone(), g.to_string());
                }
            }
        }
        let _ = std::fs::remove_file(&tmp);
    }
    out
}

/// casting.json прошлого эпизода для cross-episode матчинга: env DUB_FACES_PREV_CASTING (явный путь),
/// иначе work_dir/prev_casting.json (кладёт вызывающий/UI при выборе предыдущего эпизода). None если нет.
fn load_prev_casting(paths: &AnalyzePaths) -> Option<dub_faces::Casting> {
    if let Some(p) = std::env::var_os("DUB_FACES_PREV_CASTING") {
        if let Some(c) = load_casting(Path::new(&p)) {
            return Some(c);
        }
    }
    load_casting(&paths.work_dir.join("prev_casting.json"))
}

fn sanitize(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').collect()
}

/// Собрать speech_note персонажей в единый стилевой хвост перевода (пер-спикерный style в текущем
/// ctx_run НЕ поддержан — единый глобальный style; см. translate.rs). Формат: «CharName (реплики спикера
/// N): note». Пусто, если ни у кого нет заметки. Вызывается при применении правок кастинга (эндпоинт).
pub fn speech_notes_to_style(casting: &dub_faces::Casting) -> String {
    let mut parts: Vec<String> = Vec::new();
    for c in &casting.characters {
        let note = c.speech_note.trim();
        if note.is_empty() {
            continue;
        }
        let who = if c.name.trim().is_empty() {
            c.speaker_ids.join(",")
        } else {
            c.name.trim().to_string()
        };
        if who.is_empty() {
            parts.push(note.to_string());
        } else {
            parts.push(format!("{who}: {note}"));
        }
    }
    parts.join("; ")
}

/// Сентинел клона в позиционном CSV voice.name (#114): "-" => render клонирует голос спикера.
pub const CLONE_SLOT: &str = crate::voice_slots::CLONE_SLOT;

/// Построить новый позиционный CSV voice.name ПОВЕРХ старого при применении голосов кастинга.
/// Формат — как в render.rs/voice_slots: позиция i = i-й спикер из `sorted_speakers` (лексикографически
/// отсортированные+dedup id). Правило слияния (не затираем чужие голоса из #114):
///   - спикер есть в `vmap` со значением Some(name) -> его голос;
///   - спикер есть в `vmap` со значением None (выставлен в кастинге на 'clone') -> сентинел "-";
///   - спикера НЕТ в `vmap` -> сохраняем его позицию из старого CSV (если тот был voice-режимом),
///     иначе "-".
/// `old_csv_is_voice` = был ли proj.audio.voice.mode == "voice" (иначе старый CSV не берём).
pub fn merge_voice_csv(
    sorted_speakers: &[String],
    vmap: &HashMap<String, Option<String>>,
    old_name: Option<&str>,
    old_csv_is_voice: bool,
) -> String {
    let old_csv: Vec<&str> = old_name.unwrap_or("").split(',').map(|s| s.trim()).collect();
    let parts: Vec<String> = sorted_speakers
        .iter()
        .enumerate()
        .map(|(i, s)| match vmap.get(s) {
            Some(slot) => slot.clone().unwrap_or_else(|| CLONE_SLOT.to_string()),
            None => {
                let prev = if old_csv_is_voice { old_csv.get(i).copied() } else { None };
                match prev {
                    Some(p) if !p.is_empty() => p.to_string(),
                    _ => CLONE_SLOT.to_string(),
                }
            }
        })
        .collect();
    parts.join(",")
}

/// Мапа speaker_id -> имя голоса из кастинга (для применения per-speaker голосов тем же механизмом, что
/// voice_slots #114: voice.mode="voice" + позиционный CSV по отсортированным id). "clone"/пусто -> None.
pub fn casting_voice_map(casting: &dub_faces::Casting) -> HashMap<String, Option<String>> {
    let mut out = HashMap::new();
    for c in &casting.characters {
        let v = c.dub_voice.trim();
        let voice = if v.is_empty() || v.eq_ignore_ascii_case("clone") {
            None
        } else {
            Some(v.to_string())
        };
        for spk in &c.speaker_ids {
            out.insert(spk.clone(), voice.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use dub_faces::{Casting, Character};

    fn ch(name: &str, note: &str, voice: &str, spk: &[&str]) -> Character {
        Character {
            id: "x".into(),
            name: name.into(),
            speech_note: note.into(),
            dub_voice: voice.into(),
            speaker_ids: spk.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn speech_notes_joined_with_names() {
        let mut c = Casting::default();
        c.characters.push(ch("Босс", "грубый бас", "clone", &["0"]));
        c.characters.push(ch("", "", "clone", &["1"]));
        c.characters.push(ch("Няня", "мягко", "clone", &["2"]));
        let s = speech_notes_to_style(&c);
        assert_eq!(s, "Босс: грубый бас; Няня: мягко");
    }

    #[test]
    fn voice_map_clone_is_none() {
        let mut c = Casting::default();
        c.characters.push(ch("A", "", "clone", &["0"]));
        c.characters.push(ch("B", "", "Мужской1", &["1"]));
        let m = casting_voice_map(&c);
        assert_eq!(m.get("0"), Some(&None));
        assert_eq!(m.get("1"), Some(&Some("Мужской1".to_string())));
    }

    #[test]
    fn merge_preserves_speakers_not_in_casting() {
        // #114 назначил всем троим (voice_slots): CSV "V0,V1,V2". Кастинг трогает только спикера "1"
        // (ставит голос W1). Спикеры "0" и "2" НЕ в кастинге -> их позиции ДОЛЖНЫ сохраниться.
        let sorted = vec!["0".to_string(), "1".to_string(), "2".to_string()];
        let mut vmap: HashMap<String, Option<String>> = HashMap::new();
        vmap.insert("1".into(), Some("W1".into()));
        let csv = merge_voice_csv(&sorted, &vmap, Some("V0,V1,V2"), true);
        assert_eq!(csv, "V0,W1,V2");
    }

    #[test]
    fn merge_clone_only_for_explicit_clone_in_casting() {
        // Спикер "1" явно на 'clone' в кастинге (vmap: Some(None)) -> "-". Спикер "0" не в кастинге ->
        // сохраняем V0. Спикер "2" не в кастинге, но старого CSV на его позицию нет -> "-".
        let sorted = vec!["0".to_string(), "1".to_string(), "2".to_string()];
        let mut vmap: HashMap<String, Option<String>> = HashMap::new();
        vmap.insert("1".into(), None); // явный clone в кастинге
        let csv = merge_voice_csv(&sorted, &vmap, Some("V0,V1"), true);
        assert_eq!(csv, "V0,-,-");
    }

    #[test]
    fn merge_ignores_old_csv_when_not_voice_mode() {
        // Старый режим не "voice" (напр. "clone") -> старый CSV не берём; спикер не из кастинга -> "-".
        let sorted = vec!["0".to_string(), "1".to_string()];
        let mut vmap: HashMap<String, Option<String>> = HashMap::new();
        vmap.insert("0".into(), Some("W0".into()));
        let csv = merge_voice_csv(&sorted, &vmap, Some("junk,junk"), false);
        assert_eq!(csv, "W0,-");
    }
}
