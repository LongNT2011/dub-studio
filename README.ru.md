<div align="center">

<img src="frontend/public/favicon.svg" width="72" alt="Логотип Dub Studio"/>

# Dub Studio

**Бесплатная офлайн ИИ-студия дубляжа видео для Windows — переозвучивает любой короткий ролик на другой язык с клоном голоса, переведёнными субтитрами и локализацией вшитого текста. 100% локально, ноль Python: один нативный `.exe` (Rust + C++/CUDA); все модели и движки качаются кнопкой.**

[![License](https://img.shields.io/github/license/timoncool/dub-studio?style=flat-square)](LICENSE)
[![Stars](https://img.shields.io/github/stars/timoncool/dub-studio?style=flat-square)](https://github.com/timoncool/dub-studio/stargazers)
[![Latest release](https://img.shields.io/github/v/release/timoncool/dub-studio?include_prereleases&style=flat-square)](https://github.com/timoncool/dub-studio/releases)
[![Downloads](https://img.shields.io/github/downloads/timoncool/dub-studio/total?style=flat-square)](https://github.com/timoncool/dub-studio/releases)

[English](README.md) · **Русский** · [中文](README.zh.md) · [Español](README.es.md) · [Português](README.pt.md) · [Français](README.fr.md)

![Dub Studio — ИИ-дубляж видео на Windows](docs/screenshot.png)

</div>

## Что это

**Dub Studio** превращает любой короткий ролик в дубляж на другом языке — **с клонированным тембром спикера, переведёнными субтитрами и локализованным вшитым текстом прямо на кадре**. Закидываешь клип — умный авто-проход делает первый вариант; дальше живой редактор отдаёт под правку **каждый субтитр, голос, блюр-бокс, шрифт и тайтл** с мгновенным превью.

Всё работает **100% на твоём компьютере** — без облака, без загрузки, без подписки. Ни видео, ни голос никуда не уходят.

Это **v2 — полностью нативный порт**. Ни embeddable-Python, ни torch, ни CUDA-wheel'ов. Весь пайплайн — **Rust + нативные C++/CUDA-движки (GGUF/ONNX)**: один процесс, быстрый старт, низкое потребление VRAM. Модели, движки, CUDA/VC++-рантайм и ffmpeg приложение **скачивает и ставит само** при первом запуске — вручную нужен только драйвер NVIDIA.

## Пять режимов, переключаемых на лету

| Режим | Что делает |
|-------|------------|
| 🎙️ **Дубляж** | Полная переозвучка на целевой язык с **клоном оригинального тембра** — авто-каст по спикерам или свой голос |
| 🗣️ **Закадровый** | Перевод **поверх приглушённого оригинала** — исходник слышно снизу; баланс регулируется |
| 📝 **Субтитры** | Субтитры **на языке оригинала**, оригинальный звук сохранён — без дубляжа и перевода |
| ✨ **Шуточный ремикс** | Задай тему («как пират», «как новости») → модель **переписывает весь скрипт** и переозвучивает |
| 🎬 **Транскрипт** | Чистый **диаризованный транскрипт** с раскладкой по спикерам, караоке-плей, создание голосов в один клик, экспорт `.srt`/`.txt` |

Загрузил ролик один раз — и отправляешь в любой режим прямо в редакторе.

## Возможности

- **Клонирование голоса** — оригинальный тембр клонируется и говорит на новом языке (нативный движок [Higgs Audio v3](https://huggingface.co/bosonai), GGUF). Авто-каст по спикерам или свой голос из пака.
- **Диаризация спикеров** — кто и когда говорит (NVIDIA **Sortformer** v2, до 4 голосов), разный голос на каждого.
- **Локализация вшитого текста** — OCR детектит текст на кадре (**PP-OCR** ONNX), **блюрит оригинал** и печатает поверх локализованный титр в подобранном стиле — фича, которой нет ни у одного другого инструмента.
- **Перевод + vision-анализ стиля** — весь транскрипт переводится локально через **Gemma-4 12B** (GGUF, llama.cpp); vision-проход разбирает раскладку кадра: стиль субтитров, тайтлы, бренды, зоны текста.
- **SOTA вокал-сепарация** — **Mel-Band Roformer** (нативный BSRoformer.cpp на CUDA) отделяет голос от музыки: фон **сохраняется**, клон цепляется за чистую речь.
- **26 пресетов субтитров** — karaoke / word-by-word / hormozi / neon и другие, отрисованные **на твоём кадре** (WYSIWYG, JASSUB поверх того же `.ass`, что уходит в ffmpeg-burn).
- **Караоке-транскрипт** — воспроизводишь видео, а в транскрипте подсвечивается текущая строка и текущее **слово**.
- **Живой редактор** — правь транскрипт, голоса, стиль субтитров, блюр-боксы, тайтлы; **превью ~0.17 с/кадр**, каждая правка видна сразу.
- **Умный ре-ген** — при экспорте пересчитываются **только правленые сегменты**, а не весь ролик.
- **Пакетная обработка** — очередь файлов, все одними настройками, прогресс по каждому.
- **Сравнение до/после** — оригинал и дубляж бок о бок.
- **6 языков** — EN / RU / ZH / ES / PT / FR, авто-детект языка источника.
- **Любые видео-форматы** — MP4, MOV, MKV, WEBM, AVI и другие (декод через ffmpeg).
- **Установка в одну кнопку + авто-обновление** — модели, движки, CUDA/VC++-рантайм и ffmpeg тянутся при первом запуске; приложение обновляет себя само.
- **Полностью портативная** — ничего не пишется в профиль пользователя; удалил папку — не осталось следа.

## Скриншоты

Главный экран — пять режимов, превью выбранного видео, выбор языков, любые видео-форматы:

![Главный экран Dub Studio](docs/screenshot-home.png)

Режим «Транскрипт» — диаризованный транскрипт с раскладкой по спикерам, караоке-плеем и созданием голосов из каждого спикера в один клик:

![Режим транскрипции Dub Studio](docs/screenshot-transcribe.png)

## Требования

- **ОС:** Windows 10 / 11 (x64)
- **GPU:** NVIDIA с 8–16 ГБ VRAM
- **WebView2** — предустановлен в Windows 11 (в Windows 10 ставится автоматически)
- **Диск:** ~15 ГБ на модели, движки и рантайм (тянутся при первом запуске) + место под проекты

Вручную ставится только свежий **[драйвер NVIDIA](https://www.nvidia.com/Download/index.aspx)**. Всё остальное — модели (Higgs Audio v3, Gemma-4 12B + vision, Parakeet-TDT, Sortformer, Mel-Band Roformer), движки, CUDA-рантайм и ffmpeg — приложение скачивает кнопкой при первом запуске.

## Быстрый старт

1. **Скачай** портативную сборку из [Releases](https://github.com/timoncool/dub-studio/releases) и распакуй в любую папку (или поставь через `-setup.exe` / `.msi`).
2. **Запусти** `Dub Studio.exe`.
3. В панели **«Первый запуск»** нажми **«Скачать всё»** — приложение тянет модели, движки и рантайм (~15 ГБ, один раз). Нет драйвера NVIDIA — кнопка откроет сайт.
4. **Закинь видео**, выбери язык перевода → авто-проход делает первый вариант. Дальше правишь всё в редакторе и жмёшь **«Экспорт»**.

> Всё качается и хранится **внутри папки приложения**. Модели, кэши и проекты никуда больше не попадают.

## Как это работает

`analyze()` — фиксированный первый проход: сепарация → ASR со словными таймингами → диаризация → контекстный перевод + vision (стиль субтитров / тайтлы / бренды) → OCR (раскладка / блюр-боксы). На выходе — редактируемый документ **Project**. Каждая правка это патч Project с превью ~0.17 с/кадр; экспорт пере-прогоняет **только загрязнённые стадии**.

**Стек:** нативная оболочка **Tauri 2 (Rust)** поднимает `dub-server` (axum) на локальном порту и открывает окно на SPA — React 19 + Vite + Tailwind + react-konva поверх JASSUB. Движки: Parakeet-TDT (ASR, ONNX) · Sortformer (диаризация) · Gemma-4-12B GGUF (перевод + vision, llama.cpp) · Higgs Audio v3 (TTS) · Mel-Band Roformer (сепарация, BSRoformer.cpp) · PP-OCR (ONNX) · ffmpeg/NVENC. **Ни одного Python-процесса в рантайме.**

### Сборка из исходников

```bash
git clone https://github.com/timoncool/dub-studio.git
cd dub-studio

cd frontend && npm install && npm run build && cd ..   # 1) SPA
cargo build --release -p dub-server                     # 2) нативный сервер (axum)
cd desktop && npm install && npx tauri build            # 3) десктоп-оболочка (Tauri)
```

Требуется Node 20+, Rust (MSVC toolchain) и WebView2. Нативные движки пересобирать не нужно — приложение качает готовые.

## Другие портативные нейросети

| Проект | Описание |
|--------|----------|
| [Higgs Ultimate](https://github.com/timoncool/Higgs-Ultimate) | Нативный синтез и клонирование речи (Higgs Audio v3) |
| [ACE-Step Studio](https://github.com/timoncool/ACE-Step-Studio) | AI-студия музыки — песни, вокал, каверы, клипы |
| [Foundation Music Lab](https://github.com/timoncool/Foundation-Music-Lab) | Генерация музыки + редактор таймлайна |
| [Qwen3-TTS](https://github.com/timoncool/Qwen3-TTS_portable_rus) | Портативный TTS с клонированием голоса |
| [VibeVoice ASR](https://github.com/timoncool/VibeVoice_ASR_portable_ru) | Портативное распознавание речи |
| [SuperCaption Qwen3-VL](https://github.com/timoncool/SuperCaption_Qwen3-VL) | Портативное описание изображений |

## Авторы

- **Nerual Dreming** — [Telegram](https://t.me/nerual_dreming) | [neuro-cartel.com](https://neuro-cartel.com) | основатель [ArtGeneration.me](https://artgeneration.me)
- **Нейро-Софт** — [Telegram](https://t.me/neuroport) | портативные нейросети

## Благодарности

- **[Boson AI](https://huggingface.co/bosonai)** — модель Higgs Audio v3, и **[drbaph / Higgs-Audio-v3-Studio](https://huggingface.co/drbaph/Higgs-Audio-v3-Studio)** — GGUF-кванты и нативный движок `audiocpp_engine.dll`.
- **[NVIDIA Parakeet](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3)** и **[Sortformer](https://huggingface.co/nvidia)** — ASR и диаризация; ONNX-веса из [istupakov/parakeet-tdt-0.6b-v3-onnx](https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx) и [altunenes/parakeet-rs](https://github.com/altunenes/parakeet-rs).
- **[Google Gemma](https://huggingface.co/google/gemma-4-12b-it-qat-q4_0-gguf)** — Gemma-4 12B (перевод + vision), через [llama.cpp](https://github.com/ggml-org/llama.cpp).
- **[chenmozhijin / BSRoformer.cpp](https://github.com/chenmozhijin/BSRoformer.cpp)** и **[GaboxR67](https://huggingface.co/GaboxR67)** — нативный движок и модель Mel-Band Roformer.

## Поддержать автора

Я создаю опенсорс-софт и занимаюсь ИИ-исследованиями — большая часть в открытом доступе. Пожертвования позволяют делать и исследовать больше.

**[Все способы поддержки](DONATE.md)** | **[dalink.to/nerual_dreming](https://dalink.to/nerual_dreming)** | **[boosty.to/neuro_art](https://boosty.to/neuro_art)**

- **BTC:** `1E7dHL22RpyhJGVpcvKdbyZgksSYkYeEBC`
- **ETH (ERC20):** `0xb5db65adf478983186d4897ba92fe2c25c594a0c`
- **USDT (TRC20):** `TQST9Lp2TjK6FiVkn4fwfGUee7NmkxEE7C`

## Лицензия

Код приложения — [MIT](LICENSE). Веса моделей сохраняют свои лицензии (Higgs Audio v3 — research/non-commercial Boson AI; Gemma — Gemma Terms; и т.д.) — аудит перед каждым релизом.
