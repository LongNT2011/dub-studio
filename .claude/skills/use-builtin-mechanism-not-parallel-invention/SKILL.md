---
name: use-builtin-mechanism-not-parallel-invention
description: Use when adding, measuring, or shipping a feature in a project that already has a built-in facility (a benchmark, a chosen output format, an agreed convention) — reach for the existing mechanism instead of inventing a parallel one or silently regressing a decided choice, even if you don't explicitly remember the decision.
---

# Use the built-in mechanism — don't invent a parallel one

When a project already provides a facility, USE IT. Do not roll your own, and do not silently regress an agreed decision. Two failure modes that trigger user fury:

1. **Inventing parallel instrumentation.** If the codebase has a built-in benchmark (e.g. dub-studio `bench.rs`: `Bench::start/stage/finish` → `bench.json`, gated by `active.json "bench"`), wire your new stage into IT (`bench.stage("casting")`). Do NOT sprinkle ad-hoc `Instant::now()` timers — you'll produce numbers that don't match the real per-stage/GPU/VRAM breakdown the built-in tool emits across ALL stages.

2. **Silently regressing an agreed format/convention.** If the team decided outputs are native JPG for speed, new artifacts must be JPG — not PNG because that's what a helper defaulted to. Check the surrounding convention before picking an extension/format. `ffmpeg` and the Rust `image` crate infer format from the path extension, so the fix is usually just the filename (`char_0.jpg` not `.png`) plus any hardcoded serving route.

## Before claiming a feature is done
- **Benchmark its real cost with the built-in tool, end-to-end on a WHOLE file**, not a snippet. The user's fear is "this feature secretly eats 80% of total time." Report the honest ratio: `feature_stage.sec / (analyze.total_sec + render.total_sec)`.
- The benchmark must run **across all stages** (analyze pipeline + render), because % of total is meaningless without the total.
- Then show the finished result **от и до** (start to finish), not just the intermediate.

## Red flags (stop and use the built-in)
- "I'll just add a quick timer here" → there's already a bench; use it.
- "PNG is fine" when the project standardized on JPG → match the convention.
- "I'll measure just the casting stage" → measure the whole dub or the % is a lie.
<!-- satori: staged 2026-07-21, lesson 'correction:а-почему-блядь-аватары-у-тебя-в-пнг-стали-еблан-если-мы-решили-что-все-в-нативно', pinned 'D:\Projects\TEMP\dub-studio' -->
