# Dub Studio — карта REST/SSE контракта (порт Python → Rust)

Источник истины: `backend/app.py` (FastAPI, один GPU-воркер + asyncio-очередь). Rust-сервер
(`crates/dub-server`, axum) обязан отдавать **тот же** контракт, чтобы SPA (`frontend/dist`) работал
без правок. Порт (8765) и формат ответов совпадают.

Легенда статуса: **done** — реализовано в раунде 1; **todo** — каркас/следующие раунды (нужен движок).

## Инфраструктура запросов

- Один GPU-воркер, одна очередь джоб. Тяжёлые (GPU) операции ставятся в очередь и возвращают
  `{"job_id": "..."}`; прогресс — по SSE `GET /jobs/{id}/events`. Синхронные (не-GPU) правки Project
  отвечают сразу телом Project.
- Снапшот `OPTS` берётся в момент постановки джобы (иммунитет к конкурентному `PATCH /engine/opts`).
- В Rust: `crates/dub-server/src/jobs.rs` — единый воркер (`spawn_blocking`), broadcast-канал на SSE,
  oneshot для синхронного ожидания (preview/original). Реап терминальной джобы через 300с.

## SSE — формат событий (`GET /jobs/{id}/events`)  — **done**

```
data: {"type":"progress", "msg": "...", ...}
data: {"type":"done", "result": <json|null>}
data: {"type":"error", "error": "..."}
```
Поток завершается на `done`/`error`. 404 если job_id неизвестен. Совпадает 1:1 с app.py.

## Эндпоинты

| Метод | Путь | Статус | Назначение |
|-------|------|--------|-----------|
| GET | `/engine/capabilities` | **done** | JSON: device, tts_quant, asr_model, models{asr,llm,vision,tts}, ffmpeg(bool), languages[], voice_modes[] |
| PATCH | `/engine/opts` | todo | Свап слота модели (asr/tts/llm/vision) в рантайме; валидирует непустые строки; вернуть {models:{...}} |
| GET | `/fonts` | todo | Каталог шрифтов субтитров (family→описание) из captions.FONTS |
| GET | `/voices` | todo | Голоса voice-пака для VoicePanel (пусто если пак не установлен) |
| GET | `/presets` | todo | Пресеты вида субтитров (TEMPLATES) + список REVEALS |
| POST | `/projects` | **done** | multipart-загрузка видео → `workspace/<pid>/source.<ext>` + `source.txt`; вернуть {project_id, filename} |
| POST | `/projects/{pid}/analyze` | todo | Query: tgt_lang, mode, src_lang, subs, rewrite. Джоба: analyze()→project.json. Вернуть {job_id}. **Ядро ASR+диаризация+перевод** |
| POST | `/projects/{pid}/remix` | todo | Query: instruction. Джоба: Gemma переписывает весь транскрипт, помечает все dirty. Вернуть {job_id} |
| GET | `/projects/{pid}` | **done** | Тело Project (JSON) или 404/409 |
| PATCH | `/projects/{pid}` | todo | Синхронная правка Project (без GPU), `{op: ...}` — см. таблицу PATCH-ops ниже |
| PUT | `/projects/{pid}` | todo | Полная замена Project (undo/redo снапшот) |
| GET | `/projects/{pid}/waveform?n=600` | todo | Даунсэмпл-пики аудио (ffmpeg → s16le 8kHz), кэш в waveform.json. CPU, вне GPU-воркера |
| GET | `/projects/{pid}/preview?t=&rev=` | todo | Джоба preview_frame → PNG. Синхронное ожидание (timeout 300с), abandoned при таймауте |
| GET | `/projects/{pid}/original?t=` | todo | Джоба source_frame → PNG. Синхронное ожидание (timeout 60с) |
| POST | `/projects/{pid}/render` | todo | Джоба render()→output.mp4; regen_dub если есть dirty-сегменты; после — сбросить dirty. Вернуть {job_id} |
| GET | `/projects/{pid}/output?dl=` | todo | Отдать output.mp4 (Range для <video>); dl=1 → Content-Disposition attachment |
| GET | `/projects/{pid}/dub` | todo | Проигрываемое видео: output.mp4, иначе analyzed.mp4 (Range) |
| — (fallback) | `/{spa_path}` | **done** | SPA: реальный статик-файл (с защитой от path-traversal), иначе index.html |

## PATCH `/projects/{pid}` — операции `op` (все синхронные, без GPU) — **todo**

Правки применяются к Project, затем `project.json` перезаписывается и возвращается Project. Ошибки:
неизвестная/битая правка → 400; неизвестный seg/idx → 404.

| op | поля | действие |
|----|------|---------|
| `caption` | seg_id, + поля стиля | edit_caption; TypeError/ValueError/KeyError → 400 |
| `segment` | id, tgt_text?, src_text?, voice? | edit_segment; неизвестный id → 404 |
| `del_segment` | id | удалить строку (уходит и субтитр, и дубляж) |
| `hide_segment` | id, hidden? | тоггл/установка скрытия строки |
| `del_segments` | ids[] | массовое удаление |
| `hide_segments` | ids[], hidden | массовое скрытие (явный флаг) |
| `del_titles` | idxs[] | массовое удаление титров (high→low index) |
| `del_blurs` | idxs[] | массовое удаление blur-боксов (high→low index) |
| `keep_segment` | id, keep? | тоггл «keep original audio» (без дубляжа/перевода) |
| `keep_segments` | ids[], keep | массовый keep-original |
| `blur` | idx, + поля | edit_blur; IndexError/KeyError → 404 |
| `blur_add` | x,y,w,h,t0?,t1? | add_blur |
| `blur_del` | idx | del_blur |
| `blur_enable` | on? | глобальный тоггл блюра (render.blur) |
| `preset` | name? | имя TEMPLATE-пресета (None/"match" = как оригинал); только re-burn |
| `title` | idx, + поля | edit_title |
| `title_del` | idx | del_title |
| `title_add` | text,x,y,w,h,t0?,t1?,italic?,font?,color? | add_title |
| `subpos` | sub_y | перетащить полосу субтитров; ставит sub_y_locked=true |
| `mode` | value | set_mode (dub/nodub/transcribe); ValueError → 400 |
| `translate` | lang?, mode? | translate (Gemma; плейн/творческий) |
| `rewrite` | instruction | rewrite (творческий ремикс сегментов) |
| `recast` | voice_mode?, voice_name? | recast (сменить режим/голос дубляжа) |
| `regen` | id | пометить сегмент dirty → ре-TTS только его на /render |
| `regen_all` | — | пометить все dirty → ре-TTS всего дубляжа |

## Защита от path-traversal (SPA)  — **done**

app.py контейнит отдачу web-root: `f.is_file() and (f == WEB_R or WEB_R in f.parents)` на
канонизированном пути — `..%2f`-сегменты не должны вычитывать произвольные файлы. В Rust
(`spa.rs::serve_spa`) то же: `canonicalize()` запрошенного пути обязан начинаться с канон. web-root,
иначе → index.html. pid дополнительно ограничен `[A-Za-z0-9]` до касания ФС.

## Что реализуют движки-крейты (для analyze/render/preview следующих раундов)

- `crates/dub-asr` — Parakeet-TDT-v3 (словные таймстемпы, `TimestampMode::Words`) + Sortformer-диаризация.
  Порт `dubengine/asr.py` (`_segment`, `transcribe`, `transcribe_turns`) и `diarize.py` (turns/assign).
- `crates/audiocpp` — Higgs Audio v3 TTS/клон голоса (FFI над audiocpp_engine.dll). Порт `tts.py`/`voices.py`.
- `crates/dub-core` — типы Project (serde, extra="allow" round-trip) и EngineOpts. Порт `project.py`/`opts.py`.
- Будущие раунды: перевод/vision (llama.cpp + Gemma), separation (sherpa-onnx), OCR (RapidOcrOnnx),
  captions/рендер (ffmpeg) — `pipeline.py`, `captions.py`, `ctx_translate.py`, `compose.py`, `assemble.py`.
