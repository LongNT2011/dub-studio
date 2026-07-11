//! PATCH /projects/{pid} — синхронные правки Project без GPU. Порт первых op из dubengine/api.py +
//! app.py.patch_project. В раунде 2 реализованы: segment (edit текста/voice/hidden/keep), subpos
//! (перетащить полосу субтитров), mode (dub/nodub/transcribe через set_mode). Прочие op -> 400
//! (реализуются в следующих раундах). Ошибки: неизвестный op -> 400; неизвестный seg id -> 404.

use dub_core::Project;
use serde_json::Value;

/// Результат применения op: Ok — Project изменён; Err — (http-код, сообщение).
pub type PatchResult = Result<(), (u16, String)>;

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(|x| x.as_str()).map(|x| x.to_string())
}

/// Пометить все сегменты dirty (после смены режима/перевода re-gen на render).
fn mark_all_dirty(p: &mut Project) {
    for seg in &mut p.segments {
        seg.dirty = true;
    }
}

/// edit_segment — правка одной строки транскрипта. Порт api.edit_segment.
fn op_segment(p: &mut Project, edit: &Value) -> PatchResult {
    let sid = s(edit, "id").ok_or((400, "missing segment id".into()))?;
    let seg = p
        .segments
        .iter_mut()
        .find(|x| x.id == sid)
        .ok_or((404, format!("segment {sid:?} not found")))?;
    if let Some(t) = edit.get("tgt_text").and_then(|x| x.as_str()) {
        seg.tgt_text = t.to_string();
    }
    if let Some(t) = edit.get("src_text").and_then(|x| x.as_str()) {
        seg.src_text = t.to_string();
    }
    if let Some(v) = edit.get("voice").and_then(|x| x.as_str()) {
        seg.voice = Some(v.to_string());
    }
    // hidden / keep_original — хранятся в extra (dub-core Segment их не типизирует, но проносит).
    if let Some(h) = edit.get("hidden").and_then(|x| x.as_bool()) {
        seg.extra.insert("hidden".into(), Value::Bool(h));
    }
    if let Some(k) = edit.get("keep_original").and_then(|x| x.as_bool()) {
        seg.extra.insert("keep_original".into(), Value::Bool(k));
    }
    seg.dirty = true;
    Ok(())
}

/// subpos — перетащить полосу субтитров вертикально; ставит sub_y_locked=true (honor для всех строк).
fn op_subpos(p: &mut Project, edit: &Value) -> PatchResult {
    let sub_y = edit
        .get("sub_y")
        .and_then(|x| x.as_i64())
        .ok_or((400, "bad subpos sub_y".to_string()))?;
    p.captions.sub_y = Some(sub_y);
    p.captions.sub_y_locked = true;
    Ok(())
}

/// mode — верхнеуровневый режим вывода. Порт api.set_mode:
///   subtitles -> nodub + subs.translate; dub -> dub + subs.translate; funny -> dub + subs.translate + rewrite.
/// Помечает все сегменты dirty. ValueError (неизвестное значение) -> 400.
fn op_mode(p: &mut Project, edit: &Value) -> PatchResult {
    let value = s(edit, "value").unwrap_or_default();
    match value.as_str() {
        "subtitles" => {
            p.mode = "nodub".into();
            p.subs.mode = "translate".into();
            p.audio.rewrite = None;
        }
        "dub" => {
            p.mode = "dub".into();
            p.subs.mode = "translate".into();
            p.audio.rewrite = None;
        }
        "funny" => {
            p.mode = "dub".into();
            p.subs.mode = "translate".into();
            if p.audio.rewrite.is_none() {
                p.audio.rewrite = Some("make it a funny, playful dub".into());
            }
        }
        other => return Err((400, format!("unknown mode {other:?}"))),
    }
    mark_all_dirty(p);
    Ok(())
}

/// Применить одну PATCH-операцию к Project. op берётся из поля "op". Неизвестный op -> 400.
pub fn apply(p: &mut Project, edit: &Value) -> PatchResult {
    let op = s(edit, "op").unwrap_or_default();
    match op.as_str() {
        "segment" => op_segment(p, edit),
        "subpos" => op_subpos(p, edit),
        "mode" => op_mode(p, edit),
        other => Err((400, format!("unknown op {other:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn proj_with_seg() -> Project {
        let mut p = Project::default();
        p.segments.push(dub_core::Segment {
            id: "s0".into(),
            start: 0.0,
            end: 1.0,
            src_text: "hi".into(),
            ..Default::default()
        });
        p
    }

    #[test]
    fn segment_edits_text_and_marks_dirty() {
        let mut p = proj_with_seg();
        apply(&mut p, &json!({"op":"segment","id":"s0","tgt_text":"привет"})).unwrap();
        assert_eq!(p.segments[0].tgt_text, "привет");
        assert!(p.segments[0].dirty);
    }

    #[test]
    fn segment_unknown_id_404() {
        let mut p = proj_with_seg();
        let e = apply(&mut p, &json!({"op":"segment","id":"sX","tgt_text":"x"})).unwrap_err();
        assert_eq!(e.0, 404);
    }

    #[test]
    fn subpos_sets_locked() {
        let mut p = proj_with_seg();
        apply(&mut p, &json!({"op":"subpos","sub_y":720})).unwrap();
        assert_eq!(p.captions.sub_y, Some(720));
        assert!(p.captions.sub_y_locked);
    }

    #[test]
    fn mode_dub_and_unknown() {
        let mut p = proj_with_seg();
        apply(&mut p, &json!({"op":"mode","value":"dub"})).unwrap();
        assert_eq!(p.mode, "dub");
        assert!(p.segments[0].dirty);
        let e = apply(&mut p, &json!({"op":"mode","value":"nope"})).unwrap_err();
        assert_eq!(e.0, 400);
    }
}
