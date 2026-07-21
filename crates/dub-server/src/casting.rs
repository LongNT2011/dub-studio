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
    crop_sharpness, frontality, load_casting, occluder_path, save_casting, save_face_crop,
    AnimeFaceDetector, Casting, CcipEmbedder, Character, Face, FaceOccluder, FacesModels, LvFace,
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

/// Макс. число кадров-кандидатов на персонажа для поиска аватара (из его говорящих сегментов).
const MAX_AVATAR_CAND: usize = 24;

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

    // 3) Детектор+эмбеддер ЛИЦА по типу контента (только для аватара). Нет моделей -> без аватаров.
    let mut det = load_face_det(&paths.models_root, anime, progress);
    let mut emb = load_face_emb(&paths.models_root, anime, progress);
    // Окклюдер (FaceFusion xseg): штрафуем кадры с закрытым лицом (рука/микрофон/волосы) при выборе
    // аватара. Только для реальных лиц; отсутствие модели -> None (аватар выбирается без штрафа).
    let mut occ = if anime { None } else { load_occluder(&paths.models_root, progress) };

    let cast_dir = paths.work_dir.join("casting");
    let _ = std::fs::create_dir_all(&cast_dir);
    emit(
        progress,
        "cast_speaker",
        if anime { "аниме-детектор лиц по говорящим кадрам" } else { "детектор лиц по говорящим кадрам" },
    );

    // 4) На КАЖДОГО спикера: аватар из его говорящих кадров + face-эмбеддинг + образец голоса -> Character.
    let mut casting = Casting {
        version: CASTING_VERSION,
        content_type: if anime { "anime".into() } else { "real".into() },
        characters: Vec::new(),
    };
    for (ci, (spk, dur, lines)) in ranked.iter().enumerate() {
        let (sample_frame, face_emb) =
            avatar_for_speaker(paths, proj, spk, ci, anime, det.as_mut(), emb.as_mut(), occ.as_mut(), &cast_dir, progress);
        // образец голоса персонажа (проигрывается в UI через /casting/voice).
        if let Some(src) = speaker_samples.get(spk) {
            let dst = cast_dir.join(format!("char_{ci}_voice.wav"));
            let _ = std::fs::copy(src, &dst);
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

/// Загрузить окклюдер лица (xseg). Нет модели/не грузится -> None (аватар без штрафа за окклюзию).
fn load_occluder(models_root: &Path, progress: &Progress) -> Option<FaceOccluder> {
    let p = occluder_path(models_root);
    if !p.is_file() {
        return None;
    }
    match FaceOccluder::load(&p) {
        Ok(o) => Some(o),
        Err(e) => {
            emit(progress, "cast_speaker", &format!("окклюдер не загрузился: {e} — без штрафа за закрытое лицо"));
            None
        }
    }
}

/// Квадратный кроп лица из кадра с полями `pad` (доля от большей стороны bbox) — вход окклюдера. Клампим
/// по границам кадра; вырожденный bbox -> 1x1 (окклюдер вернёт нейтральную видимость).
fn square_face_crop(img: &image::RgbImage, bbox: (f32, f32, f32, f32), pad: f32) -> image::RgbImage {
    let (x1, y1, x2, y2) = bbox;
    let (cx, cy) = ((x1 + x2) * 0.5, (y1 + y2) * 0.5);
    let half = ((x2 - x1).max(y2 - y1) * (1.0 + pad) * 0.5).max(1.0);
    let (iw, ih) = (img.width() as f32, img.height() as f32);
    let sx = (cx - half).clamp(0.0, iw - 1.0) as u32;
    let sy = (cy - half).clamp(0.0, ih - 1.0) as u32;
    let ex = (cx + half).clamp(0.0, iw) as u32;
    let ey = (cy + half).clamp(0.0, ih) as u32;
    let w = ex.saturating_sub(sx).max(1);
    let h = ey.saturating_sub(sy).max(1);
    image::imageops::crop_imm(img, sx, sy, w, h).to_image()
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

/// Аватар персонажа: детект лица ТОЛЬКО в кадрах, где этот спикер говорит (его сегменты). Берём кадры из
/// самых длинных реплик (до MAX_AVATAR_CAND), детектим, выбираем лучшее лицо (score²·резкость·фронтальность·
/// размер) -> кроп аватара + face-эмбеддинг. Нет детектора/лиц -> ("", []). Возвращает (sample_frame, emb).
#[allow(clippy::too_many_arguments)]
fn avatar_for_speaker(
    paths: &AnalyzePaths,
    proj: &Project,
    spk: &str,
    ci: usize,
    anime: bool,
    det: Option<&mut FaceDet>,
    emb: Option<&mut FaceEmb>,
    mut occ: Option<&mut FaceOccluder>,
    cast_dir: &Path,
    progress: &Progress,
) -> (String, Vec<f32>) {
    let Some(det) = det else { return (String::new(), Vec::new()) };

    // Сегменты спикера, по убыванию длительности (в длинных репликах он крупно и на экране).
    let mut segs: Vec<(f64, f64)> = proj
        .segments
        .iter()
        .filter(|s| s.speaker.as_deref().unwrap_or("0") == spk)
        .map(|s| (s.start, s.end))
        .collect();
    segs.sort_by(|a, b| (b.1 - b.0).partial_cmp(&(a.1 - a.0)).unwrap_or(std::cmp::Ordering::Equal));

    // Таймстемпы-кандидаты: НЕ только середина (в лайв-экшене там часто motion-blur говорящего), а
    // несколько кадров внутри каждой длинной реплики (каждые ~0.6с) — больше шансов поймать резкий
    // фронтальный кадр с крупным лицом.
    let mut times: Vec<f64> = Vec::new();
    'outer: for (s, e) in &segs {
        let dur = (e - s).max(0.0);
        let n = ((dur / 0.6).floor() as usize).clamp(1, 6);
        for j in 0..n {
            let frac = (j as f64 + 0.5) / n as f64;
            times.push(s + dur * frac);
            if times.len() >= MAX_AVATAR_CAND {
                break 'outer;
            }
        }
    }

    let min_px = min_face_px();
    let tmp_dir = cast_dir.join("cand");
    let _ = std::fs::create_dir_all(&tmp_dir);
    // Лучшее лицо: (кадр, лицо, качество, лицо>=мин.размера, путь). Предпочитаем лица не мельче min_px
    // (мельче -> аватар мылится); мелкое берём только если ничего крупнее нет.
    let mut best: Option<(image::RgbImage, Face, f32, bool, std::path::PathBuf)> = None;
    for (k, t) in times.iter().enumerate() {
        let fp = tmp_dir.join(format!("s{}_{k}.png", sanitize(spk)));
        let img = match extract_frame(&paths.input, *t, &fp) {
            Ok(i) => i,
            Err(_) => continue,
        };
        let faces = match det.detect(&img) {
            Ok(f) => f,
            Err(_) => continue,
        };
        for f in faces {
            let (x1, y1, x2, y2) = (f.x1, f.y1, f.x2, f.y2);
            let side = (x2 - x1).max(0.0).min((y2 - y1).max(0.0)); // меньшая сторона bbox лица, px
            let sharp = crop_sharpness(&img, (x1, y1, x2, y2)); // variance of Laplacian: выше = резче
            // real: фронтальность по 5 точкам; anime: точек нет -> нейтрально 1.0.
            let front = if anime { 1.0 } else { frontality(&f) };
            let meets = side >= min_px;
            // Видимость (FaceFusion xseg): доля открытой кожи лица. Закрытое рукой/микрофоном/волосами лицо
            // -> низкая видимость -> штраф. Считаем только для лиц ≥ min_px (реальные кандидаты) — иначе
            // лишние прогоны xseg на мелочи. Нет окклюдера -> vis_factor=1 (без штрафа).
            let vis_factor = if meets {
                if let Some(o) = occ.as_deref_mut() {
                    let crop = square_face_crop(&img, (x1, y1, x2, y2), 0.35);
                    let vis = o.visibility(&crop).unwrap_or(1.0);
                    0.15 + 0.85 * vis
                } else {
                    1.0
                }
            } else {
                1.0
            };
            // Качество: РЕЗКОСТЬ доминирует (против расфокуса/motion-blur), размер — sublinear (sqrt),
            // чтобы крупное мыльное лицо НЕ побеждало резкое поменьше; + фронтальность × уверенность ×
            // видимость (не закрыто).
            let q = f.score * sharp * (front + 0.1) * side.sqrt() * vis_factor;
            let better = match &best {
                None => true,
                // любое лицо >= мин.размера бьёт любое мельче; при равном классе — по качеству.
                Some((_, _, bq, bmeets, _)) => (meets && !*bmeets) || (meets == *bmeets && q > *bq),
            };
            if better {
                best = Some((img.clone(), f, q, meets, fp.clone()));
            }
        }
    }

    let result = match best {
        Some((img, face, _q, _meets, fp)) => {
            let out = cast_dir.join(format!("char_{ci}.png"));
            let bbox = (face.x1, face.y1, face.x2, face.y2);
            let sample = match save_face_crop(&fp, bbox, 0.35, &out) {
                Ok(()) => format!("casting/char_{ci}.png"),
                Err(e) => {
                    emit(progress, "cast_speaker", &format!("аватар char_{ci} не сохранён: {e}"));
                    String::new()
                }
            };
            let embv = match emb {
                Some(e) => e.embed(&img, &face).unwrap_or_default(),
                None => Vec::new(),
            };
            (sample, embv)
        }
        None => (String::new(), Vec::new()),
    };
    let _ = std::fs::remove_dir_all(&tmp_dir);
    result
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
        let safe = slug.len() <= 128
            && slug.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            && !slug.contains("..");
        if safe {
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
