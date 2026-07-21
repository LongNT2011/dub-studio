---
name: dub-casting-avatars-sharp-and-in-frame
description: Use when changing dub-studio character-casting avatar selection (crates/dub-server/src/casting.rs faces_to_speakers / dub-faces cluster_faces) — the chosen avatar face MUST stay sharp, adequately sized, and fully inside the frame. The owner is adamant ("не дай бог не так").
---

# dub-studio casting avatars: sharp + in-frame, always

No matter how the face↔speaker assignment changes, the avatar shown for a character must be a CLEAN face. Hard requirements the owner enforces:

1. **Sharp** — pick the crispest frame of the assigned face identity. The quality metric in `cluster_faces` is `score² · sharpness · frontality · area_gate` (Laplacian-variance sharpness dominates, area saturating so a large blurry face never beats a smaller crisp one). Keep it.
2. **Big enough** — filter faces whose bbox short/long side is below `min_face_px()` (default 96px) BEFORE clustering; tiny faces upscale to mush at the ~250px avatar display.
3. **Fully in frame** — reject faces whose bbox touches the frame border (`x1<=1 || y1<=1 || x2>=w-1 || y2>=h-1`); an edge-cut half-face is a bad avatar.

The face↔speaker LINK (who the avatar is) and the avatar QUALITY (is it a clean crop) are separate concerns — fixing the link (e.g. co-occurrence with the diarization timeline) must NOT regress the quality gate. When you rewrite the casting face stage, re-verify all three gates survive, then confirm visually in the preview UI (open the project, look at the rendered avatars) per [[dub-verify-via-preview-ui-never-backend]] — do not judge avatars from casting.json alone.
<!-- satori: staged 2026-07-21, lesson 'correction:и-лица-блядльв-се-ещ-елоолждны-быть-четкие-и-в-кадре-нахуй-не-дай-бог-не-так-буд', pinned 'D:\Projects\TEMP\dub-studio' -->
