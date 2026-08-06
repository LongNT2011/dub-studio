---
name: dubstudio-release-build
description: Use when building or releasing dub-studio — tauri build, setup.exe/msi, portable zip, latest.json, or bumping the version. Do NOT hand-write a fresh build script from scratch.
---

# dub-studio release build

**Reuse the existing per-release script — never reinvent it.** Reinventing it wastes a build cycle and silently breaks signing.

## Steps
1. **Portable zip**: read the newest `scratchpad/pack_portable_<ver>.ps1` (highest number) and copy→bump it. It takes a base portable zip (engine DLLs/models unchanged) and swaps in fresh `Dub Studio.exe` + `frontend/dist` + `fonts`. Add any NEW sidecar files here too.
2. **Signing (critical)**: the updater key `~/.tauri/dubstudio-updater.key` **HAS a password** (stored in the `reference_dubstudio_updater` memory — NOT empty). Before `npx tauri build`:
   ```
   export TAURI_SIGNING_PRIVATE_KEY="$(cat /c/Users/user/.tauri/dubstudio-updater.key)"
   export TAURI_SIGNING_PRIVATE_KEY_PASSWORD='<from reference_dubstudio_updater memory>'
   ```
   An empty/wrong password → `failed to decode secret key: Wrong password` → no `.sig` → auto-update dead. `latest.json` embeds the setup.exe `.sig`.
3. **New sidecar binaries** (e.g. `tools/openrouter-helper/openrouter-helper.exe`): `repo_root` = the dir next to the exe (holds `frontend/`, `models/`, `fonts/`). So ship the binary in BOTH:
   - installer → `tauri.conf.json` `bundle.resources`: `"staging/tools/X": "tools/X"` + copy into `staging/`
   - portable zip → add entry at the same relative path
4. **Kill the running server first** (holds the exe lock → `os error 5` on build): `taskkill //IM dub-studio-desktop.exe //F` + free port 8765.
5. Never commit `models/active.json` (gitignored, holds the OpenRouter key) or test media.

## Release notes discipline
- Diff against the last **PUBLISHED** GitHub release tag (`gh release list`), not the last version-bump commit. Skipped/unreleased interim versions (e.g. a 2.8.0 that was tagged in-repo but never published) roll their whole content up into the current release — enumerate ALL commits `git log <last-published-tag>..HEAD` and cover every user-facing feature (it's easy to describe only the last feature and forget an earlier headline one).
- The changelog lists **new features + real improvements the user gets**, NOT internal dev fails/fixes. If no interim release shipped, a bug you introduced and fixed in WIP was never seen by anyone — do NOT write "removed invented ids", "fixed X hang", "review-loop fixes" etc. Those are noise in a changelog.
- Title should name the actual headline feature(s), even if there are two.
- If a feature was requested to be marked **beta/experimental**, carry that label to EVERY surface (changelog heading, UI, README) — not just one. Casting and autocasting are both beta.
- **Verify every capability / system-requirement claim against the code before writing it** ("runs on any PC", "works without NVIDIA", "no GPU needed"). The cloud/OpenRouter preset does NOT make the app GPU-less: it offloads only LLM/vision/TTS. Separation (BSRoformer) ships as a CUDA-only build (degrades to "no music bed" if absent), and setup still gates on the NVIDIA driver. ONNX diarization + Parakeet ASR default to the CPU provider and Whisper falls back cuda→cpu, so compute *can* run CPU — but an untested "runs anywhere" claim is a lie until proven on a real no-NVIDIA box. NVIDIA is effectively required today.

## Large files / test media (push discipline)
20+ releases pushed to master clean because `test_media/` was NEVER tracked. If `git push` is rejected with `File … exceeds GitHub's 100MB limit`, you committed test assets — you did it, not the repo. Cause: a `git add -A` / `git add .` that swept `test_media/*.mp4` (60–180 MB each) into feature commits.
- Prevention: `git add <specific paths>` only; keep `test_media/` in `.gitignore`.
- Fix (unpushed commits): `git filter-branch --force --index-filter 'git rm -r --cached --ignore-unmatch test_media' origin/master..HEAD` — strips the files from history; the on-disk files stay. Then normal (non-force) fast-forward push. Do NOT force-push master.
<!-- satori: staged 2026-07-22, lesson 'correction:так-нахуй-ты-начал-писать-нвоый-урлд-ебучуий1-7-третья-версия-увже-назху-яты-нач', pinned 'D:\Projects\TEMP\dub-studio' -->
