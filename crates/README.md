# Dub Studio — Rust-порт (crates/)

Порт Dub Studio с Python/FastAPI на нативный стек Rust (Tauri 2 + axum). Фронт (`frontend/`) НЕ
переписывается — Rust-сервер отдаёт тот же REST/SSE контракт, что `backend/app.py` (см.
`docs/PORT-CONTRACT.md`). Python-код (`backend/`, `dub-engine/`) остаётся референсом до паритета.

## Крейты

| Крейт | Назначение | Референс (Python) |
|-------|-----------|-------------------|
| `audiocpp` | FFI над `audiocpp_engine.dll` — Higgs Audio v3 TTS + клон голоса. Самостоятельный (без Tauri). | `dubengine/tts.py`, `voices.py` |
| `dub-asr` | ASR со словными таймстемпами (Parakeet-TDT-v3) + диаризация (Sortformer v2) через parakeet-rs/ONNX. | `dubengine/asr.py`, `diarize.py` |
| `dub-core` | Типы `Project` (serde, extra="allow" round-trip) и `EngineOpts`. | `dubengine/project.py`, `opts.py` |
| `dub-server` | axum: SPA-раздача (защита от traversal), `/engine/capabilities`, `/projects` (upload), SSE-джобы. | `backend/app.py` |

## Сборка

```powershell
# PowerShell (cargo виден сразу)
cargo build --workspace
```
```bash
# Git Bash: сначала прокинуть cargo в PATH
export PATH="$(cygpath -u "$USERPROFILE/.cargo/bin"):$PATH"
cargo build --workspace --examples
```

## Модели и рантайм (не коммитятся, лежат в `models/`)

- **onnxruntime.dll** — `dub-asr` собран с `ort/load-dynamic`: onnxruntime грузится в рантайме.
  Указывается через `ORT_DYLIB_PATH=<...>\onnxruntime.dll`. **КРИТИЧНО: строго onnxruntime 1.24.x**
  (ort 2.0-rc.12 собран под 1.24.2). DLL версий 1.22/1.23 вызывают ДЕДЛОК в `commit_from_file`
  (рассинхрон OrtApi, поток блокируется наглухо, без ошибки). Официальный билд:
  github.com/microsoft/onnxruntime releases → `onnxruntime-win-x64-1.24.2.zip` → `lib/onnxruntime.dll`.
- **Parakeet-TDT-0.6b-v3 (ONNX), int8** — дефолт Higgs-Ultimate `tdt-0.6b-v3-int8` (★ recommended,
  ~670МБ). HF `istupakov/parakeet-tdt-0.6b-v3-onnx`, файлы:
  `encoder-model.int8.onnx`, `decoder_joint-model.int8.onnx`, `vocab.txt` (+`config.json`, `nemo128.onnx`).
  Каталог `models/tdt/`. parakeet-rs автоопределяет имена; когда в папке ТОЛЬКО .int8.onnx — грузится int8
  (тот же 8-битный дефолт, что в Python-референсе `asr.py` `quantization="int8"`). **int8 требует Level1**
  (см. ниже): дефолтный parakeet-rs Level3 виснет при создании CPU-сессии на квант-узлах
  (DynamicQuantizeLinear/MatMulInteger). GPU-провайдер (`--features cuda`) снимает ограничение.
- **Sortformer v2** — HF `altunenes/parakeet-rs`, `diar_streaming_sortformer_4spk-v2.onnx` → `models/sortformer/`.
- **Higgs Audio v3 веса, Q8_0** — дефолт Higgs-Ultimate `higgs-q8_0` (★ recommended, gguf ~5.4ГБ). HF
  `drbaph/Higgs-Audio-v3-Studio`, путь `models/higgs-q8_0/`: `q8_0.gguf` + `config.json`,
  `chat_template.jinja`, `tokenizer.json`, `tokenizer_config.json`, `higgs_audio_v2_tokenizer_config.json`.
  Локально `models/higgs-q8_0/` (`--model-root`). DLL движка — `models/higgs-engine/audiocpp_engine.dll`
  (из `drbaph/.../engines/audiocpp_engine.dll` или релиза Higgs-Ultimate).

## Примеры

### ASR (словные таймстемпы + опц. диаризация)
```bash
export ORT_DYLIB_PATH="D:\Projects\TEMP\dub-studio\models\runtime\onnxruntime.dll"
# транскрипция:
./target/release/examples/asr --wav in.wav --tdt models/tdt
# с диаризацией:
./target/release/examples/asr --wav in.wav --tdt models/tdt --diarize --sortformer models/sortformer/diar_streaming_sortformer_4spk-v2.onnx
```
Вывод: JSON `{"segments":[{start,end,text,words:[{word,start,end}]}]}` (или `{turns, segments}` с --diarize).

`DUB_ASR_OPT_LEVEL` (0..3, дефолт 1) — уровень оптимизации графа ONNX. **Важно:** на int8-кванте
дефолтный parakeet-rs Level3 виснет при создании CPU-сессии на минуты; крейт понижает до Level1.
GPU-провайдер (`--features cuda` + CUDA EP) снимает это ограничение.

### synth (TTS / клон голоса через Higgs DLL)
```bash
./target/release/examples/synth --dll <audiocpp_engine.dll> --model-root <higgs_weights> \
    --text "Привет" --out out.wav --backend cpu
# клон голоса:
./target/release/examples/synth --dll ... --model-root ... --text "..." \
    --ref-wav ref.wav --ref-text "референсный текст" --out out.wav
```

### Сервер
```bash
DUB_STUDIO_ROOT=<repo> ./target/release/dub-server   # слушает 127.0.0.1:8765
```

## Статус (раунд 1)

Собрано и проверено: `cargo build --workspace --examples` зелёный; `dub-core` round-trip тесты и
`dub-asr` сегментация — проходят; ASR-пример прогнан на реальном речевом WAV. Каркас сервера (capabilities,
upload, get project, SSE-джобы, SPA). GPU-эндпоинты analyze/render/preview — следующие раунды
(карта в `docs/PORT-CONTRACT.md`).
