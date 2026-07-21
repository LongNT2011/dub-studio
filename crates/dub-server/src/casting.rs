//! Стадия кастинга персонажей (#115) для analyze — SPEAKER-DRIVEN.
//!
//! Первичка — АУДИО/диаризация: персонаж = СПИКЕР (спикеры отсортированы по суммарному времени речи —
//! главные герои первыми). Голос (WeSpeaker: эмбеддинг + образец-фраза) — первичный кастинг. Лицо/аватар
//! ДОПОЛНЯЕТ голос: детектим лицо ТОЛЬКО в кадрах, ГДЕ ЭТОТ СПИКЕР ГОВОРИТ (его сегменты), берём лучший.
//! Никакого скана всего видео и кластеризации лиц (см. feedback_casting_speaker_driven_not_clustering).
//!
//! content_type: "real" -> SCRFD (лица) + LVFace (512-d, косинус); "anime" -> anime_face_detection
//! (рисованные лица) + CCIP (768-d, L2). Кросс-эпизод матчим только одинаковый content_type.
//!
//! ГЕЙТ: только при proj.casting_enabled. Fail-safe: нет весов лица -> персонажи строятся по голосу без
//! аватара; сбой не валит analyze (кастинг не блокер).

use std::collections::HashMap;
use std::path::Path;

use dub_core::{Project, Segment};
use dub_faces::{
    crop_sharpness, frontality, load_casting, save_casting, save_face_crop,
    AnimeFaceDetector, Casting, CcipEmbedder, Character, Face, FacesModels, LvFace,
    Scrfd, CASTING_VERSION,
};

use crate::analyze::{AnalyzePaths, Progress};

#[cfg(windows)]
const FFMPEG: &str = "ffmpeg.exe";
#[cfg(not(windows))]
const FFMPEG: &str = "ffmpeg";

// Стадии кастинга для степпера фронта: cast_detect (подготовка/спикеры) -> cast_embed (голос) ->
// cast_speaker (аватарки из говорящих кадров + casting.json).
fn emit(progress: &Progress, stage: &str, msg: &str) {
    progress(serde_json::json!({ "stage": stage, "msg": msg }));
}

/// Мин. сторона bbox лица (px) для «чёткого» аватара: мельче — кроп мылится при показе на ~250px.
/// Предпочитаем лица >= этого; мельче берём лишь если крупнее не нашлось. Env DUB_FACES_MIN_FACE_PX.
fn min_face_px() -> f32 {
    std::env::var("DUB_FACES_MIN_FACE_PX").ok().and_then(|s| s.trim().parse().ok()).unwrap_or(96.0)
}

/// Порог косинуса голосовой кластеризации сегментов (WeSpeaker). Выше -> больше персонажей (строже
/// различает голоса). Env DUB_VOICE_CLUSTER_COS. 0 или отсутствие модели -> без переразметки.
fn voice_cluster_cos() -> f32 {
    // Порог остановки AHC average-linkage (косинус центроидов WeSpeaker). 0.3 подтверждён на чистом аудио
    // (Avatar live-action: 4 Sortformer -> 10 персонажей, мужские найдены). Env DUB_VOICE_CLUSTER_COS.
    std::env::var("DUB_VOICE_CLUSTER_COS").ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0.3)
}

/// Мин. длительность сегмента (сек) для голосового эмбеддинга (короче — шумный вектор, размечаем соседом).
const VC_MIN_SEG_SEC: f64 = 0.6;

/// Агломеративная кластеризация AVERAGE-LINKAGE (по центроидам): сливаем два ближайших по косинусу
/// кластера (центроид = среднее эмбеддингов членов), пока max-косинус >= threshold. Центроиды денойзят
/// шум коротких сегментов -> сходится к истинному числу спикеров ЛУЧШЕ single-linkage (тот на шумном
/// аудио фрагментирует). Возвращает метки 0..k-1 по первому появлению. O(n³) в худшем — n сегментов немного.
fn ahc_average(embs: &[Vec<f32>], threshold: f32) -> Vec<usize> {
    let n = embs.len();
    if n == 0 {
        return Vec::new();
    }
    let dim = embs[0].len();
    let mut members: Vec<Vec<usize>> = (0..n).map(|i| vec![i]).collect();
    let mut centroids: Vec<Vec<f32>> = embs.to_vec();
    let mut active: Vec<bool> = vec![true; n];
    loop {
        let idxs: Vec<usize> = (0..centroids.len()).filter(|&i| active[i]).collect();
        let mut best = (usize::MAX, usize::MAX, f32::MIN);
        for a in 0..idxs.len() {
            for b in (a + 1)..idxs.len() {
                let (i, j) = (idxs[a], idxs[b]);
                let c = dub_asr::cosine(&centroids[i], &centroids[j]);
                if c > best.2 {
                    best = (i, j, c);
                }
            }
        }
        if best.0 == usize::MAX || best.2 < threshold {
            break;
        }
        let (i, j) = (best.0, best.1);
        let jm = std::mem::take(&mut members[j]);
        members[i].extend(jm);
        active[j] = false;
        // центроид i = среднее эмбеддингов всех членов.
        let mut c = vec![0.0f32; dim];
        for &pt in &members[i] {
            for d in 0..dim {
                c[d] += embs[pt][d];
            }
        }
        let cnt = members[i].len() as f32;
        for v in &mut c {
            *v /= cnt;
        }
        centroids[i] = c;
    }
    let mut labels = vec![0usize; n];
    let mut next = 0usize;
    for i in 0..centroids.len() {
        if active[i] {
            for &pt in &members[i] {
                labels[pt] = next;
            }
            next += 1;
        }
    }
    labels
}

/// Голосовая кластеризация СЕГМЕНТОВ (#83 global speakers, по указанию юзера): Sortformer даёт ≤4 спикеров
/// ОДНОВРЕМЕННО в окне, но за всё видео их больше. Эмбеддим вокал каждого сегмента (WeSpeaker) и
/// кластеризуем по cosine -> присваиваем КАЖДОМУ сегменту глобальную голосовую метку "v{k}". Находит
/// персонажей больше Sortformer'а (слитых/пропущенных, напр. мужского) и сливает один голос из разных окон
/// в одного. Возвращает КЛОН проекта с переразмеченными segment.speaker; None -> нет модели/вокала/1 кластер
/// (оставляем Sortformer-разметку). Дорого (эмбеддинг на сегмент), но только при включённом кастинге.
/// Голосовая переразметка спикеров (#83/#115): Sortformer даёт ≤4 в окне, а персонажей больше —
/// кластеризуем вокал по сегментам (WeSpeaker average-linkage) и ПЕРЕРАЗМЕЧАЕМ сами сегменты метками
/// «v{k}» НА МЕСТЕ. Зовётся в пайплайне ПОСЛЕ ASR (до перевода/TTS) -> дубляж идёт по N голосам, счётчик
/// реплик у персонажей верный, кастинг строится на тех же метках (0 реплик больше не бывает). Возвращает
/// число кластеров (0 = не размечали: выключено env / нет модели / не дало больше персонажей, чем
/// Sortformer). ВКЛ по умолчанию; opt-out DUB_VOICE_REDIARIZE=0. Порог DUB_VOICE_CLUSTER_COS.
/// Детерминированный отпечаток раскладки сегментов (FNV-1a по границам, мс) — ключ инвалидации кэша
/// эмбеддингов при изменении числа/границ сегментов между билдами.
fn segments_fingerprint(segs: &[Segment]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for s in segs {
        for t in [s.start, s.end] {
            let ms = (t * 1000.0).round() as i64 as u64;
            h ^= ms;
            h = h.wrapping_mul(0x100000001b3);
        }
    }
    h
}

pub fn recluster_segments(paths: &AnalyzePaths, segments: &mut [Segment], progress: &Progress) -> usize {
    if std::env::var("DUB_VOICE_REDIARIZE").ok().as_deref() == Some("0") {
        return 0;
    }
    if segments.len() < 2 {
        return 0;
    }
    let vocals = {
        let clean = paths.work_dir.join("vocals16_clean.wav");
        let raw = paths.work_dir.join("vocals16.wav");
        if clean.is_file() {
            clean
        } else if raw.is_file() {
            raw
        } else {
            return 0;
        }
    };
    let onnx = std::env::var("DUB_FACES_WESPEAKER")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| dub_faces::wespeaker_path(&paths.models_root));
    if !onnx.is_file() {
        return 0;
    }
    // Кэш эмбеддингов (тюнинг порога без пере-эмбеддинга): casting/vc_embs.json = {seg_count, fp, idxs, embs}.
    // fp — отпечаток ГРАНИЦ сегментов: если константы merge_short_turns изменились между билдами и дали иную
    // раскладку при СОВПАВШЕМ count, fp разойдётся -> кэш инвалидируется (idxs не мапятся на чужие сегменты).
    let cache = paths.work_dir.join("casting").join("vc_embs.json");
    let fp = segments_fingerprint(segments);
    let mut idxs: Vec<usize> = Vec::new();
    let mut embs: Vec<Vec<f32>> = Vec::new();
    let cached = std::fs::read_to_string(&cache)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .filter(|v| {
            v.get("seg_count").and_then(|x| x.as_u64()) == Some(segments.len() as u64)
                && v.get("fp").and_then(|x| x.as_u64()) == Some(fp)
        });
    if let Some(v) = cached {
        idxs = serde_json::from_value(v.get("idxs").cloned().unwrap_or_default()).unwrap_or_default();
        embs = serde_json::from_value(v.get("embs").cloned().unwrap_or_default()).unwrap_or_default();
    }
    if idxs.is_empty() || idxs.len() != embs.len() {
        // Эмбеддим вокал каждого достаточно длинного сегмента.
        idxs.clear();
        embs.clear();
        let mut embedder = match dub_faces::VoiceEmbedder::load(&onnx) {
            Ok(e) => e,
            Err(_) => return 0,
        };
        let tmp = paths.work_dir.join("casting_vc.wav");
        for (i, s) in segments.iter().enumerate() {
            if (s.end - s.start) < VC_MIN_SEG_SEC {
                continue;
            }
            let end = s.end.min(s.start + 6.0).max(s.start + 0.05);
            if crate::media::trim(&vocals, &tmp, s.start, end, 16_000).is_err() {
                continue;
            }
            if let Ok(v) = embedder.embed_wav(&tmp) {
                idxs.push(i);
                embs.push(v);
            }
        }
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::create_dir_all(cache.parent().unwrap_or(&paths.work_dir));
        let _ = std::fs::write(
            &cache,
            serde_json::json!({ "seg_count": segments.len(), "fp": fp, "idxs": idxs, "embs": embs }).to_string(),
        );
    }
    if embs.len() < 2 {
        return 0;
    }
    let max_chars: usize = std::env::var("DUB_VOICE_MAX_CHARS")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .filter(|&n: &usize| n >= 2)
        .unwrap_or(8);
    let mut thr = voice_cluster_cos();
    let mut labels = ahc_average(&embs, thr);
    let mut k = labels.iter().copied().max().map(|m| m + 1).unwrap_or(0);
    // Кап на число персонажей: 13 голосов на 16 мин = переразбиение (один персонаж дробится). Пока
    // кластеров слишком много — снижаем порог (сливает близкие голоса), до разумного числа или пола 0.18.
    while k > max_chars && thr > 0.18 {
        thr -= 0.02;
        labels = ahc_average(&embs, thr);
        k = labels.iter().copied().max().map(|m| m + 1).unwrap_or(0);
    }
    // Число исходных Sortformer-меток (борроу сразу отпускаем — ниже мутируем segments).
    let orig_count = segments
        .iter()
        .filter_map(|s| s.speaker.as_ref())
        .collect::<std::collections::HashSet<&String>>()
        .len();
    if k <= 1 || k <= orig_count {
        // Кластеризация не дала БОЛЬШЕ персонажей, чем Sortformer -> не переразмечаем (не рискуем).
        return 0;
    }
    // Sortformer УВЕРЕННО сказал один спикер (orig_count<=1): дробим ТОЛЬКО если новые кластеры реально
    // населены. Настоящий второй голос имеет заметную долю реплик; горстка сегментов в отдельном кластере —
    // это разброс просодии ОДНОГО человека (крик/шёпот), а не второй персонаж. Иначе монолог рвётся на
    // v0/v1 и озвучивается двумя голосами (регресс снятия n_spk-гейта). Требуем 2-й кластер >=2 и >=15%.
    if orig_count <= 1 {
        let mut sizes = vec![0usize; k];
        for &l in &labels {
            sizes[l] += 1;
        }
        sizes.sort_unstable_by(|a, b| b.cmp(a));
        let second = sizes.get(1).copied().unwrap_or(0);
        let min_needed = 2.max((labels.len() as f64 * 0.15).ceil() as usize);
        if second < min_needed {
            return 0; // разброс просодии одного спикера, не второй голос
        }
    }
    // seg_idx -> голосовая метка (для эмбеддированных).
    let mut seg_label: HashMap<usize, usize> = HashMap::new();
    for (j, &i) in idxs.iter().enumerate() {
        seg_label.insert(i, labels[j]);
    }
    // Короткие/невекторизованные сегменты -> метка ближайшего по времени эмбеддированного. Центры считаем
    // заранее (иначе borrow segments и immut, и mut одновременно).
    let mids: Vec<f64> = segments.iter().map(|s| (s.start + s.end) / 2.0).collect();
    for i in 0..segments.len() {
        let lbl = seg_label.get(&i).copied().or_else(|| {
            let mid = mids[i];
            idxs.iter()
                .min_by(|&&a, &&b| {
                    (mids[a] - mid).abs().partial_cmp(&(mids[b] - mid).abs()).unwrap_or(std::cmp::Ordering::Equal)
                })
                .and_then(|&j| seg_label.get(&j).copied())
        });
        if let Some(l) = lbl {
            segments[i].speaker = Some(format!("v{l}"));
        }
    }
    emit(
        progress,
        "asr",
        &format!("голосовая переразметка: {k} персонажей по голосу (Sortformer нашёл {orig_count})"),
    );
    k
}

/// Прогнать стадию кастинга. proj уже собран транскрипт-стадией (segments + speaker). Пишет casting.json
/// в work_dir и аватарки в work_dir/casting/. `casting_ref` — slug профиля app-библиотеки для применения
/// к этому ролику; `content_type` — "real"|"anime".
pub fn stage(paths: &AnalyzePaths, proj: &Project, casting_ref: &str, content_type: &str, progress: &Progress) {
    if !proj.casting_enabled {
        return;
    }
    if proj.segments.is_empty() {
        emit(progress, "cast_detect", "кастинг пропущен: нет речевых сегментов");
        return;
    }
    let anime = content_type.eq_ignore_ascii_case("anime");

    // 0) Сегменты УЖЕ переразмечены по голосу в пайплайне (recluster_segments после ASR) — метки «v{k}».
    //    Персонаж = спикер (по этим меткам) => те же метки на сегментах => счётчик реплик верный, дубляж
    //    идёт по N голосам. Здесь никакой переразметки: работаем по proj.segments как есть.

    // 1) СПИКЕРЫ по суммарному времени речи (главные персонажи первыми) — первичка кастинга.
    let ranked = speakers_by_talktime(proj);
    if ranked.is_empty() {
        emit(progress, "cast_detect", "кастинг пропущен: нет спикеров");
        return;
    }
    let ids: Vec<String> = ranked.iter().map(|(s, _, _)| s.clone()).collect();
    emit(progress, "cast_detect", &format!("персонажей-спикеров: {} (ранжированы по времени речи)", ranked.len()));

    // 2) Пол (F0) + голос (WeSpeaker: 256-d эмбеддинг + образец-фраза) — per-speaker, аудио. F0 считаем и
    // для аниме: после голосовой кластеризации каждый кластер — ОДИН голос, F0 на его чистой реплике
    // надёжнее (мужской кластер -> мужской). Неуверенный замер всё равно -> "" (gender_label не гадает).
    let genders = speaker_genders(paths, proj, &ids);
    let (voice_embeddings, speaker_samples) = speaker_voices(paths, proj, &ids, progress);

    // 3) Детектор+эмбеддер ЛИЦА по типу контента. Нет моделей -> без аватаров.
    let mut det = load_face_det(&paths.models_root, anime, progress);
    let mut emb = load_face_emb(&paths.models_root, anime, progress);

    let cast_dir = paths.work_dir.join("casting");
    let _ = std::fs::create_dir_all(&cast_dir);

    // 3b) ПРИВЯЗКА ЛИЦО↔СПИКЕР по СО-ВСТРЕЧАЕМОСТИ (best-practice: face-recognition + diarization fusion,
    // MERL/AVA-AVD). Собираем лица по всему говорящему таймлайну, узнаём одно и то же лицо по LVFace-ВЕКТОРУ
    // (cluster_faces), считаем ДИСКРИМИНАТИВНУЮ со-встречаемость с таймлайном реплик (кто/когда говорит уже
    // известен из диаризации) и назначаем каждому спикеру ЕГО повторяющееся лицо. Слушатель, мелькающий у
    // ВСЕХ спикеров, давится. Аватар берётся из этого лица, а не «самое резкое в кадре» — лечит корневой баг
    // «аватар = слушатель». Возвращает speaker_id -> (медоид-эмбеддинг, путь аватара).
    let face_map = match (det.as_mut(), emb.as_mut()) {
        (Some(d), Some(e)) => faces_to_speakers(paths, proj, &ranked, d, e, anime, &cast_dir, progress),
        _ => std::collections::HashMap::new(),
    };
    emit(progress, "cast_speaker", &format!("лиц привязано к спикерам: {} из {}", face_map.len(), ranked.len()));

    // 4) На КАЖДОГО спикера: аватар из его говорящих кадров + face-эмбеддинг + образец голоса -> Character.
    let mut casting = Casting {
        version: CASTING_VERSION,
        content_type: if anime { "anime".into() } else { "real".into() },
        characters: Vec::new(),
    };
    for (ci, (spk, dur, lines)) in ranked.iter().enumerate() {
        // Аватар+эмбеддинг лица из привязки со-встречаемости (3b); нет лица у спикера (закадровый) -> пусто.
        let (face_emb, sample_frame) = face_map.get(spk).cloned().unwrap_or_default();
        // образец голоса персонажа (проигрывается в UI через /casting/voice). Путь пишем в voice_sample —
        // резолвится по нему, а не по id (match может сменить id на профильный).
        let mut voice_sample = String::new();
        if let Some(src) = speaker_samples.get(spk) {
            let dst = cast_dir.join(format!("char_{ci}_voice.wav"));
            if std::fs::copy(src, &dst).is_ok() {
                voice_sample = format!("casting/char_{ci}_voice.wav");
            }
        }
        let has_face = !sample_frame.is_empty();
        emit(
            progress,
            "cast_speaker",
            &format!("char_{ci}: спикер {spk}, реплик {lines}, речь {dur:.0}с, лицо: {}", if has_face { "да" } else { "нет" }),
        );
        casting.characters.push(Character {
            id: format!("char_{ci}"),
            gender: genders.get(spk).cloned().unwrap_or_default(),
            speaker_ids: vec![spk.clone()],
            face_embedding: face_emb,
            voice_embedding: voice_embeddings.get(spk).cloned().unwrap_or_default(),
            sample_frame,
            voice_sample,
            ..Default::default()
        });
    }

    // 5) cross-episode / применение библиотеки — ТОЛЬКО одинаковый content_type (разные пространства
    //    эмбеддингов: real=512 косинус, anime=768 L2). Матч по face+voice similarity (character_similarity).
    if let Some(prev) = load_prev_casting(paths, casting_ref, progress) {
        let prev_type = if prev.content_type.is_empty() { "real" } else { prev.content_type.as_str() };
        let cur_type = if anime { "anime" } else { "real" };
        if prev_type != cur_type {
            emit(
                progress,
                "cast_speaker",
                &format!("профиль другого типа ({prev_type} ≠ {cur_type}) — кросс-матч пропущен"),
            );
        } else {
            let mt = dub_faces::match_cos_threshold();
            casting.characters = dub_faces::match_cross_episode(&casting.characters, &prev, mt);
            let carried = casting.characters.iter().filter(|c| !c.name.is_empty()).count();
            emit(progress, "cast_speaker", &format!("cross-episode: перенесено имён/голосов: {carried}"));
        }
    }

    // 6) записать casting.json.
    let path = paths.work_dir.join("casting.json");
    match save_casting(&path, &casting) {
        Ok(()) => emit(progress, "cast_speaker", &format!("casting.json готов: {} персонаж(ей)", casting.characters.len())),
        Err(e) => emit(progress, "cast_speaker", &format!("не удалось записать casting.json: {e}")),
    }
}

/// Привязка лицо↔спикер по СО-ВСТРЕЧАЕМОСТИ. Собираем лица по всему говорящему таймлайну, узнаём одно и то
/// же лицо по LVFace-вектору (cluster_faces), считаем дискриминативную со-встречаемость кластеров с реплик-
/// таймлайном спикеров (link_faces_discriminative) и назначаем каждому спикеру ЕГО лицо. Аватар = медоидный
/// кадр кластера. Возвращает speaker_id -> (face_embedding, путь аватара "casting/char_<idx>.png").
#[allow(clippy::too_many_arguments)]
fn faces_to_speakers(
    paths: &AnalyzePaths,
    proj: &Project,
    ranked: &[(String, f64, usize)],
    det: &mut FaceDet,
    emb: &mut FaceEmb,
    anime: bool,
    cast_dir: &Path,
    progress: &Progress,
) -> HashMap<String, (Vec<f32>, String)> {
    use dub_faces::{FrameFace, SpeakerTurn};
    let mut out: HashMap<String, (Vec<f32>, String)> = HashMap::new();

    // 1) времена-кандидаты: сэмплируем кадры ВНУТРИ речевых сегментов (~каждые SAMPLE_SEC), кап MAX_SAMPLES.
    const SAMPLE_SEC: f64 = 0.7;
    const MAX_SAMPLES: usize = 500;
    let mut times: Vec<f64> = Vec::new();
    'outer: for s in &proj.segments {
        let dur = (s.end - s.start).max(0.0);
        let n = ((dur / SAMPLE_SEC).floor() as usize).max(1);
        for j in 0..n {
            times.push(s.start + (j as f64 + 0.5) * dur / n as f64);
            if times.len() >= MAX_SAMPLES {
                break 'outer;
            }
        }
    }

    // 2) детект + эмбед по каждому кадру -> FrameFace[] (+ путь PNG кадра для сохранения аватара).
    let tmp = cast_dir.join("cand");
    let _ = std::fs::create_dir_all(&tmp);
    let min_px = min_face_px();
    let mut faces: Vec<FrameFace> = Vec::new();
    let mut face_frame: Vec<std::path::PathBuf> = Vec::new();
    for (k, t) in times.iter().enumerate() {
        let fp = tmp.join(format!("f{k}.png"));
        let img = match extract_frame(&paths.input, *t, &fp) {
            Ok(i) => i,
            Err(_) => continue,
        };
        let dets = match det.detect(&img) {
            Ok(d) => d,
            Err(_) => continue,
        };
        let (iw, ih) = (img.width() as f32, img.height() as f32);
        for f in dets {
            let (x1, y1, x2, y2) = (f.x1, f.y1, f.x2, f.y2);
            let side = (x2 - x1).max(0.0).max((y2 - y1).max(0.0));
            if side < min_px {
                continue; // мелкие/фоновые лица не дают устойчивого эмбеддинга (и мылятся в аватаре)
            }
            // ГАРД «в кадре»: лицо, обрезанное краем кадра, — плохой аватар (полулицо). Отсекаем касающиеся
            // border на 1px. Резкость/фронтальность/размер добьёт cluster_faces при выборе аватара кластера.
            if x1 <= 1.0 || y1 <= 1.0 || x2 >= iw - 1.0 || y2 >= ih - 1.0 {
                continue;
            }
            let embv = emb.embed(&img, &f).unwrap_or_default();
            if embv.is_empty() {
                continue;
            }
            let sharp = crop_sharpness(&img, (x1, y1, x2, y2));
            let front = if anime { 1.0 } else { frontality(&f) };
            faces.push(FrameFace {
                t: *t,
                bbox: (x1, y1, x2, y2),
                score: f.score,
                sharpness: sharp,
                frontality: front,
                embedding: embv,
            });
            face_frame.push(fp.clone());
        }
    }
    emit(progress, "cast_speaker", &format!("лиц собрано: {} (сэмплов {})", faces.len(), times.len()));
    if faces.len() < 2 {
        let _ = std::fs::remove_dir_all(&tmp);
        return out;
    }

    // 3) кластеризация по ВЕКТОРУ -> лица-персоны (медоид + кадр-аватар).
    let clusters = dub_faces::cluster_faces(&faces, dub_faces::cluster_cos_threshold());
    emit(progress, "cast_speaker", &format!("лиц-персон (кластеров по вектору): {}", clusters.len()));

    // 4) дискриминативная со-встречаемость с таймлайном реплик -> назначение кластер->спикер.
    let turns: Vec<SpeakerTurn> = proj
        .segments
        .iter()
        .filter_map(|s| s.speaker.as_ref().map(|spk| SpeakerTurn { start: s.start, end: s.end, speaker: spk.clone() }))
        .collect();
    let speakers: Vec<String> = ranked.iter().map(|(s, _, _)| s.clone()).collect();
    let cluster_times: Vec<Vec<f64>> = clusters
        .iter()
        .map(|c| {
            let mut ts: Vec<f64> = c.members.iter().map(|&i| faces[i].t).collect();
            ts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            ts
        })
        .collect();
    let linkage = dub_faces::link_faces_discriminative(&cluster_times, &turns, &speakers);

    // 5) speaker -> cluster; аватар = save_face_crop(медоидного кадра кластера). char-индекс = позиция
    //    спикера в ranked (совпадает с ci в stage -> имя файла char_<idx>.png консистентно).
    for (ci, cl) in clusters.iter().enumerate() {
        let Some(spk) = linkage.get(&ci) else {
            continue;
        };
        let idx = speakers.iter().position(|s| s == spk).unwrap_or(0);
        let av = cl.avatar; // индекс FrameFace-аватара кластера
        let out_png = cast_dir.join(format!("char_{idx}.png"));
        let rel = match save_face_crop(&face_frame[av], faces[av].bbox, 0.35, &out_png) {
            Ok(()) => format!("casting/char_{idx}.png"),
            Err(_) => String::new(),
        };
        out.insert(spk.clone(), (cl.embedding.clone(), rel));
    }
    let _ = std::fs::remove_dir_all(&tmp);
    out
}

/// Спикеры, отсортированные по суммарной длительности речи (desc), с числом реплик. Главные персонажи
/// первыми. Tie-break по id (детерминизм). Возвращает (speaker_id, суммарная_длит_сек, число_реплик).
fn speakers_by_talktime(proj: &Project) -> Vec<(String, f64, usize)> {
    let mut acc: HashMap<String, (f64, usize)> = HashMap::new();
    for s in &proj.segments {
        let spk = s.speaker.clone().unwrap_or_else(|| "0".to_string());
        let e = acc.entry(spk).or_insert((0.0, 0));
        e.0 += (s.end - s.start).max(0.0);
        e.1 += 1;
    }
    let mut v: Vec<(String, f64, usize)> = acc.into_iter().map(|(k, (d, n))| (k, d, n)).collect();
    v.sort_by(|a, b| {
        b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.0.cmp(&b.0))
    });
    v
}

/// Детектор лиц по типу контента. real -> SCRFD; anime -> anime_face_detection. Нет модели/сбой -> None
/// (персонажи строятся по голосу без аватара, не падаем).
enum FaceDet {
    Real(Scrfd),
    Anime(AnimeFaceDetector),
}
impl FaceDet {
    fn detect(&mut self, img: &image::RgbImage) -> Result<Vec<Face>, String> {
        match self {
            FaceDet::Real(s) => s.detect(img),
            FaceDet::Anime(a) => a.detect(img),
        }
    }
}

/// Эмбеддер лица по типу контента. real -> LVFace (512-d, кроп лица+align); anime -> CCIP (768-d, кроп
/// всего персонажа).
enum FaceEmb {
    Lv(LvFace),
    Ccip(CcipEmbedder),
}
impl FaceEmb {
    fn embed(&mut self, img: &image::RgbImage, face: &Face) -> Result<Vec<f32>, String> {
        match self {
            FaceEmb::Lv(l) => l.embed_face(img, face),
            FaceEmb::Ccip(c) => c.embed_character(img, face),
        }
    }
}

fn load_face_det(models_root: &Path, anime: bool, progress: &Progress) -> Option<FaceDet> {
    if anime {
        let p = models_root.join("faces").join("anime_face").join("model.onnx");
        if !p.is_file() {
            emit(progress, "cast_detect", &format!("аниме-детектор не найден ({}) — без аватаров", p.display()));
            return None;
        }
        match AnimeFaceDetector::load(&p) {
            Ok(d) => Some(FaceDet::Anime(d)),
            Err(e) => {
                emit(progress, "cast_detect", &format!("аниме-детектор не загрузился: {e} — без аватаров"));
                None
            }
        }
    } else {
        let models = FacesModels::resolve(models_root);
        if !models.scrfd.is_file() {
            emit(progress, "cast_detect", "SCRFD не найден — без аватаров (кастинг по голосу)");
            return None;
        }
        match Scrfd::load(&models.scrfd) {
            Ok(d) => Some(FaceDet::Real(d)),
            Err(e) => {
                emit(progress, "cast_detect", &format!("SCRFD не загрузился: {e} — без аватаров"));
                None
            }
        }
    }
}

fn load_face_emb(models_root: &Path, anime: bool, progress: &Progress) -> Option<FaceEmb> {
    if anime {
        let p = dub_faces::ccip_path(models_root);
        if !p.is_file() {
            emit(progress, "cast_embed", &format!("CCIP не найден ({}) — аватар без эмбеддинга", p.display()));
            return None;
        }
        match CcipEmbedder::load(&p) {
            Ok(e) => Some(FaceEmb::Ccip(e)),
            Err(e) => {
                emit(progress, "cast_embed", &format!("CCIP не загрузился: {e} — аватар без эмбеддинга"));
                None
            }
        }
    } else {
        let models = FacesModels::resolve(models_root);
        if !models.lvface.is_file() {
            return None;
        }
        match LvFace::load(&models.lvface) {
            Ok(e) => Some(FaceEmb::Lv(e)),
            Err(e) => {
                emit(progress, "cast_embed", &format!("LVFace не загрузился: {e} — аватар без эмбеддинга"));
                None
            }
        }
    }
}

/// Извлечь один кадр видео в момент t (сек) -> RgbImage. Быстрый seek (-ss ПЕРЕД -i).
fn extract_frame(video: &Path, t: f64, out: &Path) -> Result<image::RgbImage, String> {
    let res = std::process::Command::new(FFMPEG)
        .args(["-y", "-ss"])
        .arg(format!("{:.3}", t.max(0.0)))
        .arg("-i")
        .arg(video)
        .args(["-frames:v", "1"])
        .arg(out)
        .output()
        .map_err(|e| format!("ffmpeg: {e}"))?;
    if !res.status.success() {
        return Err("ffmpeg не извлёк кадр".into());
    }
    Ok(image::open(out).map_err(|e| format!("open frame: {e}"))?.to_rgb8())
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
                if let Some(g) = gender_label(f) {
                    out.insert(spk.clone(), g.to_string());
                }
            }
        }
        let _ = std::fs::remove_file(&tmp);
    }
    out
}

/// Границы длины образца-фразы для голосового эмбеддера (сек). RESEARCH: чистый вокал 16к, длиннейшая
/// реплика спикера, 2-6с; короче 1с — шумнее, но ок. Кап сверху 6с, пол — 1с.
const VOICE_SAMPLE_MAX_SEC: f64 = 6.0;
const VOICE_SAMPLE_MIN_SEC: f64 = 1.0;

/// Голосовые эмбеддинги + wav-образцы для КАЖДОГО спикера (WeSpeaker 256-d). Длиннейшая реплика на чистом
/// вокале -> casting/char-<spk>_voice.wav + эмбеддинг. Нет модели/вокала/клип<1с -> спикер без голоса
/// (не падаем). Возвращает (speaker_id -> эмбеддинг, speaker_id -> путь к wav-образцу).
fn speaker_voices(
    paths: &AnalyzePaths,
    proj: &Project,
    speakers: &[String],
    progress: &Progress,
) -> (HashMap<String, Vec<f32>>, HashMap<String, std::path::PathBuf>) {
    let mut embs: HashMap<String, Vec<f32>> = HashMap::new();
    let mut samples: HashMap<String, std::path::PathBuf> = HashMap::new();

    let vocals = {
        let clean = paths.work_dir.join("vocals16_clean.wav");
        let raw = paths.work_dir.join("vocals16.wav");
        if clean.is_file() {
            clean
        } else if raw.is_file() {
            raw
        } else {
            emit(progress, "cast_embed", "голосовой эмбеддинг пропущен: нет чистого вокала");
            return (embs, samples);
        }
    };

    let onnx = std::env::var("DUB_FACES_WESPEAKER")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| dub_faces::wespeaker_path(&paths.models_root));
    if !onnx.is_file() {
        emit(progress, "cast_embed", &format!("голос пропущен: нет модели WeSpeaker ({})", onnx.display()));
        return (embs, samples);
    }
    let mut embedder = match dub_faces::VoiceEmbedder::load(&onnx) {
        Ok(e) => e,
        Err(e) => {
            emit(progress, "cast_embed", &format!("WeSpeaker не загрузился: {e}; голос пропущен"));
            return (embs, samples);
        }
    };

    let cast_dir = paths.work_dir.join("casting");
    let _ = std::fs::create_dir_all(&cast_dir);
    for spk in speakers {
        let cand = proj
            .segments
            .iter()
            .filter(|s| s.speaker.as_deref().unwrap_or("0") == spk)
            .max_by(|a, b| {
                (a.end - a.start).partial_cmp(&(b.end - b.start)).unwrap_or(std::cmp::Ordering::Equal)
            });
        let Some(seg) = cand else { continue };
        if (seg.end - seg.start) < VOICE_SAMPLE_MIN_SEC {
            continue;
        }
        let wav = cast_dir.join(format!("char-{}_voice.wav", sanitize(spk)));
        let end = seg.end.min(seg.start + VOICE_SAMPLE_MAX_SEC).max(seg.start + 0.05);
        if let Err(e) = crate::media::trim(&vocals, &wav, seg.start, end, 16_000) {
            emit(progress, "cast_embed", &format!("образец голоса {spk}: обрезка не удалась: {e}"));
            continue;
        }
        match embedder.embed_wav(&wav) {
            Ok(v) => {
                embs.insert(spk.clone(), v);
                samples.insert(spk.clone(), wav);
            }
            Err(e) => {
                emit(progress, "cast_embed", &format!("голос {spk}: эмбеддинг не удался: {e}"));
                samples.insert(spk.clone(), wav);
            }
        }
    }
    if !embs.is_empty() {
        emit(progress, "cast_embed", &format!("голосовых эмбеддингов: {}", embs.len()));
    }
    (embs, samples)
}

/// casting.json для cross-episode матчинга. Приоритет: 1) `casting_ref` (slug профиля app-библиотеки);
/// 2) env DUB_FACES_PREV_CASTING; 3) work_dir/prev_casting.json. None если ничего нет.
fn load_prev_casting(
    paths: &AnalyzePaths,
    casting_ref: &str,
    progress: &Progress,
) -> Option<Casting> {
    let slug = casting_ref.trim();
    if !slug.is_empty() {
        // Единый slug-guard (общий с load_profile_casting/delete/avatar) — не дублируем предикаты, чтобы
        // при ужесточении is_safe_slug не осталось дыры на этом пути.
        if crate::casting_library::is_safe_slug(slug) {
            let prof = paths.repo_root.join("casting_library").join(slug).join("casting.json");
            if let Some(c) = load_casting(&prof) {
                emit(progress, "cast_speaker", &format!("применяю профиль библиотеки: {slug}"));
                return Some(c);
            }
        }
        emit(progress, "cast_speaker", &format!("профиль библиотеки «{slug}» не найден — без применения"));
    }
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

/// Собрать speech_note персонажей в единый стилевой хвост перевода. Формат: «CharName: note». Пусто, если
/// ни у кого нет заметки. Вызывается при применении правок кастинга (эндпоинт).
pub fn speech_notes_to_style(casting: &Casting) -> String {
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

/// Построить новый позиционный CSV voice.name ПОВЕРХ старого при применении голосов кастинга (не затираем
/// чужие голоса #114). См. тесты. `old_csv_is_voice` = был ли proj.audio.voice.mode == "voice".
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

/// Мапа speaker_id -> имя голоса из кастинга (для применения per-speaker голосов). "clone"/пусто -> None.
pub fn casting_voice_map(casting: &Casting) -> HashMap<String, Option<String>> {
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
        let sorted = vec!["0".to_string(), "1".to_string(), "2".to_string()];
        let mut vmap: HashMap<String, Option<String>> = HashMap::new();
        vmap.insert("1".into(), Some("W1".into()));
        let csv = merge_voice_csv(&sorted, &vmap, Some("V0,V1,V2"), true);
        assert_eq!(csv, "V0,W1,V2");
    }

    #[test]
    fn merge_clone_only_for_explicit_clone_in_casting() {
        let sorted = vec!["0".to_string(), "1".to_string(), "2".to_string()];
        let mut vmap: HashMap<String, Option<String>> = HashMap::new();
        vmap.insert("1".into(), None);
        let csv = merge_voice_csv(&sorted, &vmap, Some("V0,V1"), true);
        assert_eq!(csv, "V0,-,-");
    }

    #[test]
    fn merge_ignores_old_csv_when_not_voice_mode() {
        let sorted = vec!["0".to_string(), "1".to_string()];
        let mut vmap: HashMap<String, Option<String>> = HashMap::new();
        vmap.insert("0".into(), Some("W0".into()));
        let csv = merge_voice_csv(&sorted, &vmap, Some("junk,junk"), false);
        assert_eq!(csv, "W0,-");
    }

    #[test]
    fn talktime_ranks_by_duration_desc() {
        use dub_core::Segment;
        let mut p = Project::default();
        let seg = |spk: &str, a: f64, b: f64| Segment {
            start: a,
            end: b,
            speaker: Some(spk.to_string()),
            ..Default::default()
        };
        p.segments = vec![seg("1", 0.0, 1.0), seg("0", 1.0, 6.0), seg("0", 6.0, 8.0), seg("1", 8.0, 8.5)];
        let r = speakers_by_talktime(&p);
        assert_eq!(r[0].0, "0"); // 7с речи -> первый
        assert_eq!(r[0].2, 2); // 2 реплики
        assert_eq!(r[1].0, "1"); // 1.5с -> второй
    }
}
