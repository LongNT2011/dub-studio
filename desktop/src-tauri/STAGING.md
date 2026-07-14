# Bundle staging (`desktop/src-tauri/staging/`)

Каталог `staging/` — источник для `bundle.resources` в `tauri.conf.json`. Он **генерируется** перед
`npx tauri build` и **не коммитится** (в `.gitignore`). Собирает то, что должно лечь рядом с `.exe` в
NSIS/MSI-установщике и в портативной раскладке: SPA, шрифты и **бандл-компоненты** (VC++-рантайм +
OCR-модели). Сам сервер **встроен в `Dub Studio.exe`** (один процесс, `dub-server` — path-зависимость
оболочки, поднимается `serve_blocking` с фонового потока) — отдельного `dub-server.exe` больше нет.
Модели/движки/CUDA/ffmpeg сюда **не** кладутся — они качаются при первом запуске (см.
`crates/dub-server/src/setup.rs`, `delivery: Download`).

## Как собрать staging

Из корня репозитория, после `cd frontend && npm run build` (сервер собирать отдельно не нужно — он
линкуется в оболочку при `tauri build`):

```bash
STAGE=desktop/src-tauri/staging
rm -rf "$STAGE"
mkdir -p "$STAGE/models/higgs-engine" "$STAGE/models/ocr" "$STAGE/frontend"
cp -r frontend/dist                       "$STAGE/frontend/dist"
cp -r fonts                               "$STAGE/fonts"
# Бандл: VC++ runtime (delivery=Bundled)
cp models/higgs-engine/MSVCP140.dll models/higgs-engine/VCOMP140.DLL \
   models/higgs-engine/VCRUNTIME140.dll models/higgs-engine/VCRUNTIME140_1.dll \
   "$STAGE/models/higgs-engine/"
# Бандл: OCR-модели (delivery=Bundled)
cp models/ocr/det.onnx models/ocr/cls.onnx \
   models/ocr/rec_cyrillic.onnx models/ocr/rec_cyrillic.dict.txt \
   models/ocr/rec_ch.onnx models/ocr/rec_ch.dict.txt \
   "$STAGE/models/ocr/"
```

Затем `cd desktop && npx tauri build` — NSIS (`-setup.exe`) и MSI (`_en-US.msi`) появятся в
`desktop/src-tauri/target/release/bundle/{nsis,msi}/`.

## Раскладка после установки

Установщик кладёт ресурсы **рядом с `Dub Studio.exe`** (та же раскладка, что портатив). Оболочка
(`desktop/src-tauri/src/lib.rs::resolve_repo_root`) видит рядом каталоги `frontend/` и `models/` и берёт
этот каталог за `DUB_STUDIO_ROOT`; встроенный сервер находит `frontend/dist`, `fonts`,
`models/higgs-engine`, `models/ocr` относительно него. Остальное (модели/движки) докачивается в тот же
каталог по кнопке «Первый запуск».

> **Портатив** дополнительно несёт **полный** `models/higgs-engine/` (audiocpp + CUDA-DLL, ~560 МБ), а не
> только VC++-рантайм, — чтобы Higgs TTS работал без докачки движка. Собирается отдельным шагом (копия
> `Dub Studio.exe` + `fonts` + `frontend/dist` + локальные `models/higgs-engine` и `models/ocr`), затем
> 7-Zip в `Dub Studio_2.0.0_x64_Portable.zip`.
