// Compatibility bridge for UI strings that are still hard-coded in Russian.
// Keeps English mode readable without changing Russian mode or native language names.
(() => {
  const pairs = [
    // Models / engines / OpenRouter
    ["Введите ключ OpenRouter ниже (Облачные настройки)", "Enter the OpenRouter key below (Cloud settings)"],
    ["Авто", "Auto"],
    ["— выбрать TTS-модель —", "— select a TTS model —"],
    ["⚠ Модель не поддерживает русский — выберите другую для русского дубляжа.", "⚠ This model does not support Russian — choose another one for Russian dubbing."],
    ["Автокастинг голосов", "Automatic voice casting"],
    ["бета", "beta"],
    ["Голос по полу спикера автоматически, разным спикерам — разные.", "Assign voices automatically by speaker gender; different speakers get different voices."],
    ["— один голос на всех —", "— one voice for all —"],
    ["— выбрать STT-модель —", "— select an STT model —"],
    ["Транскрипция через облако — тяжёлые локальные ASR-модели качать не нужно.", "Cloud transcription — no need to download large local ASR models."],
    ["— выбрать модель перевода —", "— select a translation model —"],
    ["Vision-анализ кадров через облако", "Cloud vision analysis of frames"],
    ["как модель перевода", "same as translation model"],
    ["Ключ рабочий — сохранён", "Key verified — saved"],
    ["Ключ не принят OpenRouter", "OpenRouter rejected the key"],
    ["OpenRouter недоступен", "OpenRouter is unavailable"],
    ["Ключ OpenRouter", "OpenRouter key"],
    ["для облачных движков (перевод / TTS)", "for cloud engines (translation / TTS)"],
    ["Проверить", "Verify"],

    // Hardware / performance
    ["Пресет под железо", "Hardware preset"],
    ["NVIDIA GPU не найдена", "NVIDIA GPU not found"],
    ["Ресурсы", "Resources"],
    ["Питание", "Power"],
    ["Процесс", "Process"],
    ["Оторвать монитор ресурсов", "Pop out resource monitor"],
    ["Вернуть в шапку", "Dock back to header"],

    // Quality / TTS settings
    ["Авто-проверка услышанного текста через Whisper ASR для отсечения тишины и дефектов", "Automatically verify generated speech with Whisper ASR to detect silence and defects"],
    ["Проверка текста через ASR (QC)", "Speech verification via ASR (QC)"],
    ["авто-сверка озвучки через ASR (отключение ускоряет синтез)", "automatically verify generated speech with ASR (turning this off speeds up synthesis)"],
    ["Проверка текста через ASR", "Speech verification via ASR"],
    ["Подгонка скорости и контроль хронометража аудио под рамки субтитра", "Adjust speech speed and timing to fit the subtitle window"],
    ["Контроль длительности фраз (Stretch QC)", "Phrase duration control (Stretch QC)"],
    ["подгонка хронометража и максимального растяжения", "adjust timing and maximum stretch"],
    ["Контроль длительности фраз", "Phrase duration control"],
    ["Генерировать 3 варианта озвучки каждой фразы и автоматически выбирать лучший по таймингу", "Generate 3 takes for each line and automatically choose the best-timed one"],
    ["Multi-take отбор (3 дубля)", "Multi-take selection (3 takes)"],
    ["3 варианта озвучки — выбирается лучший по таймингу (медленнее, но качественнее)", "3 voice takes — the best-timed one is selected (slower, but higher quality)"],
    ["Multi-take отбор", "Multi-take selection"],
    ["Динамическая адаптация темпа генерации нейросети под длину текста и доступный временной слот", "Dynamically adapt TTS speaking rate to the text length and available time slot"],
    ["Динамический темп речи (Speech Rate TTS)", "Dynamic speech rate (Speech Rate TTS)"],
    ["адаптация скорости выговора нейросети под длину текста в окне", "adapt TTS speaking speed to the text length in the time window"],
    ["Динамический темп речи", "Dynamic speech rate"],
    ["Перенос эмоций, интонации и подачи прямо из оригинального звука сцены", "Transfer emotion, intonation, and delivery directly from the original scene audio"],
    ["Эмоциональный референс сцены (Emo-Ref)", "Scene emotion reference (Emo-Ref)"],
    ["копирование интонации, эмоции и подачи оригинала сцены", "copy the original scene's intonation, emotion, and delivery"],
    ["Эмоциональный референс сцены", "Scene emotion reference"],
    ["Автоматическая подстановка тихих естественных вдохов в паузах между репликами", "Automatically insert quiet, natural breaths in pauses between lines"],
    ["Вставка дыханий между фразами", "Insert breaths between phrases"],
    ["подстановка естественных мягких вдохов в паузах для оживления речи", "insert natural soft breaths in pauses to make speech sound more alive"],
    ["Вставка дыханий", "Breath insertion"],

    // Start screen / manual project flow
    ["Создать проект (ручная настройка)", "Create project (manual setup)"],
    ["ручная настройка", "manual setup"],
    ["Создать проект без автогенераций и сразу перейти к субтитрам", "Create a project without auto-generation and go straight to subtitles"],
    ["Ручной режим", "Manual mode"],
    ["к субтитрам", "to subtitles"],
    ["Создание проекта в ручном режиме...", "Creating project in manual mode..."],
    ["Создан проект в ручном режиме — готово к работе с субтитрами", "Manual project created — ready to edit subtitles"],

    // Subtitle/editor UI and notifications
    ["Приглушать фон под голосом в дубляже. Выкл — фон на полной громкости.", "Duck the background under dubbed speech. Off keeps the background at full volume."],
    ["Приглушать фон под голосом", "Duck background under speech"],
    ["Дакинг: фон тише под речью дубляжа. Выкл — фон полный.", "Ducking: lower the background under dubbed speech. Off keeps it full."],
    ["Размытая подложка под субтитрами для читаемости. Выкл — текст без подложки.", "Blurred subtitle background for readability. Off shows text without a background."],
    ["Блюр-подложка под субтитрами", "Blurred subtitle background"],
    ["Размытая подложка под субтитрами. Выкл — без подложки.", "Blurred background behind subtitles. Off disables the background."],
    ["Импорт субтитров (.srt / .ass)", "Import subtitles (.srt / .ass)"],
    ["Файл субтитров успешно сохранён!", "Subtitle file saved successfully!"],
    ["Все изменения субтитров успешно сохранены!", "All subtitle changes saved successfully!"],
    ["Файл субтитров пуст или не удалось распознать формат (.srt/.ass)", "The subtitle file is empty or its format could not be recognized (.srt/.ass)"],
    ["(пак не найден)", "(voice pack not found)"],
    ["— автокастинг по полу —", "— auto-cast by gender —"],

    // Command palette
    ["⌘K  —  команды…", "⌘K  —  commands…"]
  ];

  const ruToEn = new Map(pairs);
  const enToRu = new Map(pairs.map(([ru, en]) => [en, ru]));

  function isEnglish() {
    return (document.documentElement.lang || "en").toLowerCase().startsWith("en");
  }

  function convertExact(value, toEnglish) {
    const map = toEnglish ? ruToEn : enToRu;
    return map.get(value) || value;
  }

  function convertDynamic(value, toEnglish) {
    if (toEnglish) {
      return value
        .replace(/(\d+(?:[.,]\d+)?)\s*ГБ\s+VRAM/g, "$1 GB VRAM")
        .replace(/^ОЗУ\s+(\d+(?:[.,]\d+)?|\?)\s*ГБ$/, "RAM $1 GB")
        .replace(/(\d+(?:[.,]\d+)?)\s*ГБ/g, "$1 GB")
        .replace(/(\d+(?:[.,]\d+)?)\s*Вт/g, "$1 W")
        .replace(/^Спикер\s+(.+)$/, "Speaker $1")
        .replace(/^Загружены субтитры:\s*(\d+)\s+фраз\s+из\s+(.+)$/, "Imported subtitles: $1 lines from $2");
    }
    return value
      .replace(/(\d+(?:[.,]\d+)?)\s*GB\s+VRAM/g, "$1 ГБ VRAM")
      .replace(/^RAM\s+(\d+(?:[.,]\d+)?|\?)\s*GB$/, "ОЗУ $1 ГБ")
      .replace(/(\d+(?:[.,]\d+)?)\s*GB/g, "$1 ГБ")
      .replace(/(\d+(?:[.,]\d+)?)\s*W/g, "$1 Вт")
      .replace(/^Speaker\s+(.+)$/, "Спикер $1")
      .replace(/^Imported subtitles:\s*(\d+)\s+lines\s+from\s+(.+)$/, "Загружены субтитры: $1 фраз из $2");
  }

  function convertValue(value, toEnglish) {
    return convertDynamic(convertExact(value, toEnglish), toEnglish);
  }

  function translateTextNode(node, toEnglish) {
    const raw = node.nodeValue || "";
    const match = raw.match(/^(\s*)([\s\S]*?)(\s*)$/);
    if (!match || !match[2]) return;
    const next = convertValue(match[2], toEnglish);
    if (next !== match[2]) node.nodeValue = `${match[1]}${next}${match[3]}`;
  }

  function translateElement(el, toEnglish) {
    if (el.nodeType !== Node.ELEMENT_NODE) return;
    for (const attr of ["title", "placeholder", "aria-label"]) {
      if (!el.hasAttribute(attr)) continue;
      const current = el.getAttribute(attr) || "";
      const next = convertValue(current, toEnglish);
      if (next !== current) el.setAttribute(attr, next);
    }
  }

  function walk(root) {
    const toEnglish = isEnglish();
    if (root.nodeType === Node.TEXT_NODE) {
      translateTextNode(root, toEnglish);
      return;
    }
    if (root.nodeType !== Node.ELEMENT_NODE && root.nodeType !== Node.DOCUMENT_FRAGMENT_NODE) return;
    if (root.nodeType === Node.ELEMENT_NODE) translateElement(root, toEnglish);
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT | NodeFilter.SHOW_TEXT);
    let node;
    while ((node = walker.nextNode())) {
      if (node.nodeType === Node.TEXT_NODE) translateTextNode(node, toEnglish);
      else translateElement(node, toEnglish);
    }
  }

  const observer = new MutationObserver((mutations) => {
    for (const mutation of mutations) {
      if (mutation.type === "attributes" && mutation.target === document.documentElement && mutation.attributeName === "lang") {
        walk(document.body);
        continue;
      }
      if (mutation.type === "characterData") translateTextNode(mutation.target, isEnglish());
      if (mutation.type === "attributes") translateElement(mutation.target, isEnglish());
      for (const node of mutation.addedNodes) walk(node);
    }
  });

  function start() {
    walk(document.body);
    observer.observe(document.documentElement, {
      subtree: true,
      childList: true,
      characterData: true,
      attributes: true,
      attributeFilter: ["lang", "title", "placeholder", "aria-label"]
    });
  }

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", start, { once: true });
  else start();
})();
