---
name: dub-verify-via-preview-ui-never-backend
description: Use when verifying or testing ANY dub-studio result (casting, dubbing, transcript, subtitles, translation) — do it through the preview UI as a real user, NEVER by reading workspace files with node/ffprobe or hitting the server. The user has raged about this repeatedly.
---

# Verify dub-studio ONLY through the preview UI, as a user

The user's hardest, most-repeated rule for dub-studio: **prove the app works by DRIVING and OBSERVING the preview UI, like a user would.** Reading `workspace/<id>/project.json` or `casting.json` with `node`, or running `ffprobe`/`ffmpeg volumedetect` on output files, and presenting those numbers as "the test" — this is the same offense as curl. It makes him furious ("опять говорю ... без курла ... как юзер").

## How to apply
- **Casting result** → open the editor's casting panel in the preview and LOOK: characters listed, each with a real face avatar (not hands/phone), a voice sample play button. Screenshot it. Don't `cat casting.json`.
- **Dubbing** → play segments in the preview player / per-phrase "Прослушать озвучку", watch the karaoke highlight, confirm RU audio lines up. Don't ffprobe the mux.
- **Transcript / subtitles** → read them in the transcript UI, scrub the timeline. Don't diff project.json.
- Use `preview_snapshot` / `preview_screenshot` / `preview_click` / `preview_eval` (DOM reads only) — the browser tools ARE the test harness here.
- Backend file reads are allowed ONLY as internal orchestration timing (e.g. "is the job done yet"), NEVER as the reported verification — and even then prefer watching the UI progress %.
- Before any browser re-test: rebuild the frontend bundle + hard-reset cache first (stale bundle = false result).

Related: [[feedback_testing_like_user]], [[feedback_rebuild_frontend_clear_cache_before_browser_test]], [[feedback_no_sleep_playwright]].
<!-- satori: staged 2026-07-21, lesson 'correction:и-я-блядь-опять-говорю-тестирукеншь-через-превью-бюез-курла-все-как-юзер-мудак-т', pinned 'D:\Projects\TEMP\dub-studio' -->
