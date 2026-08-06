---
name: verify-whole-feature-with-short-test-clip
description: Use when about to declare a dub-studio feature done or kick off a release, or the moment you catch yourself calling a real E2E "too heavy/slow/many segments" — finish and verify the WHOLE feature end-to-end first, and get real data by taking a short ffmpeg cut of an existing test_media file.
---

# Verify the whole feature on a real short clip — never excuse-out the E2E

When the user says "доделай / проверь что всё доделал / не бросил на полпути / потом релиз", they mean: **every** promised piece is finished AND proven end-to-end before you build the release. Enumerate the pieces (each backend path + each UI surface + each edge case) and check each — don't verify one path and call it done.

## Get real test data — don't declare it "too heavy"
- Transcription in dub-studio is **near-instant**. There is a folder of `test_media/*.mp4`. If an existing workspace project is large (e.g. 743 segments), that is NOT a reason to skip the test.
- Take a **short segment** of a test file and run the real flow on it:
  `ffmpeg -y -i test_media/<file>.mp4 -t 20 -c copy <scratch>/clip.mp4`
  then `createProject` → `analyze` (transcribe) → the feature under test.
- Saying "N segments is too heavy for a quick test" is the exact excuse the user rages at. Cut a clip and run it.

## Don't mutate the user's real projects while testing
Copy a workspace project (or make a fresh one from a clip) to a throwaway pid; never run an in-place mutating endpoint (retranslate, patch, render) against the user's real `workspace/<pid>` during a test.

Related: [[verify-through-app-ui-not-scripts]], [[pre-release-check]], [[verify-from-clean-state-not-dev-machine]].
<!-- satori: staged 2026-07-22, lesson 'correction:и-остальное-блядь-првоерь-что-ты-все-доделал-и-не-бросил-на-пол-пути-и-сделай-по', pinned 'D:\Projects\TEMP\dub-studio' -->
