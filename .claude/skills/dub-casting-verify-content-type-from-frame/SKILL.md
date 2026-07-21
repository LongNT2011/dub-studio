---
name: dub-casting-verify-content-type-from-frame
description: Use when choosing the dub-studio casting content_type (real vs anime/cartoon) or a face-detection path, or when picking casting models for a video — verify the ACTUAL footage, never infer live-action-vs-animation from the show's title.
---

# Casting content_type: look at a frame, don't guess from the title

dub-studio casting has two paths: `content_type="real"` (SCRFD `det_10g.onnx` + LVFace `LVFace-L_Glint360K.onnx`, cosine) for photographic human faces, and `content_type="anime"` (anime_face detector + CCIP) for drawn/cartoon characters.

**Never pick the path from the title.** Many franchises exist as BOTH animation and live-action — e.g. *Avatar: The Last Airbender* is a cartoon AND a Netflix live-action series with real actors. Assuming "anime" from the name and reaching for the anime models on live-action footage is wrong (the user's live-action Avatar test is `real`).

## How to apply
- Before choosing content_type, **sample a frame** (`ffmpeg -ss <t> -i in.mp4 -frames:v 1 f.png`) and look at it — real photographed faces → `real`; drawn/animated → `anime`.
- Model paths resolve **directly** under `<repo>/models/faces/`, NOT in `scrfd/`/`lvface/` subdirs: `models/faces/det_10g.onnx` + `models/faces/LVFace-L_Glint360K.onnx` (real), `models/faces/anime_face/model.onnx` + `models/faces/ccip/model_feat.onnx` (anime), plus `models/faces/wespeaker/…` (voice) and `models/faces/lr-asd/…` (active-speaker). Check the real file path before concluding a model is "missing".
- SCRFD/LVFace are NOT in the download manifest (see setup.rs) — they're placed manually; if absent, casting falls back to voice-only (no avatars).

Related: [[feedback_never_guess_verify_only]], [[handoff_casting_115_2026_07_18]].
<!-- satori: staged 2026-07-21, lesson 'correction:модели-аниме-пути-причем-тут-аниме-это-сериалд-с-людьми-блядь-актерами', pinned 'D:\Projects\TEMP\dub-studio' -->
