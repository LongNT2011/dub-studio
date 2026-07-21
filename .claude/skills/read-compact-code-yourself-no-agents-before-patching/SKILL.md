---
name: read-compact-code-yourself-no-agents-before-patching
description: Use when investigating or fixing a bug in a compact codebase (dub-studio and similar single-app repos) — read the ACTUAL relevant source fully into your OWN context and understand the real control flow BEFORE editing, and do NOT spawn agents/workflows to investigate a small app. Also: never publish a release until the user explicitly says go.
---

# Read the compact code yourself, understand it, THEN patch

For a compact app (like dub-studio — a handful of Rust files), the fast, cheap, correct move is to **read the relevant source directly into your own context and trace the actual control flow** before changing anything.

## What went wrong (the lesson)
Investigating why auto-casting over-merged (`v1` = 84/95 lines), I grepped a little and **guessed twice** — first "single-linkage chaining", then "threshold too low" — and started patching each time. Both were wrong: the code already used `ahc_average` (centroid average-linkage), and the real root was a **cap loop that lowered the AHC threshold until k≤max_chars, which collapses average-linkage into one giant cluster**. The user: *"разберись сначала в коде долбоеоб, ты рефактор лупы делал?"* and *"у тебя 1 файл всё приложение, всоси его сам целиком в контекст без агентов сука"*.

## Rules
1. **Read the whole relevant subsystem first** (the function AND its callers/helpers), don't grep-a-fragment → hypothesize → patch. Trace the code until you can state the exact mechanism.
2. **Do NOT spawn agents / Workflow to investigate a compact codebase.** It wastes tokens and is slower than reading the ~1–5 files yourself. Fan-out is for genuinely large/parallel work, not for understanding one app.
3. **Verify the hypothesis empirically before coding the fix** — e.g. replay the exact algorithm on the real cached data (here: run the clustering on `casting/vc_embs.json` at several thresholds) to SEE the failure and confirm the fix shape, rather than assuming.
4. **A red flag = you're about to edit after only grepping.** Stop and read the function end-to-end first.

## Release gate (same session, separate correction)
Never `gh release create` / publish / deploy until the user **explicitly** says go — even inside an autonomous "finish the marathon" instruction. The marathon's "ship a release" step still waits for the user's explicit green light, especially while the core feature isn't yet user-approved. *"релиз ты публикуешь тогда, когда я тебе разрежу"*. See [[feedback_no_deploy_without_command]].
<!-- satori: staged 2026-07-21, lesson 'correction:так-разберись-сначала-в-коде-долбоеоб-ты-рефактор-лупы-делал', pinned 'D:\Projects\TEMP\dub-studio' -->
