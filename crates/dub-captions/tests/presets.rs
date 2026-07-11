//! Юнит-тесты на генерацию ASS для нескольких пресетов (сравнение ожидаемых тэгов). Проверяем, что
//! каждый лук эмитит характерные для него ASS-теги (karaoke \kf, word reveal \t..\alpha, hormozi
//! highlight recolour, neon glow-плашка), плюс базовый head/стили.

use dub_captions::{build, set_fonts_dir, BuildArgs, Sub, SubStyle};
use std::path::PathBuf;

fn fonts_dir() -> PathBuf {
    // тесты запускаются из корня крейта -> ../../fonts (repo/fonts).
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("../../fonts");
    p
}

fn gen(caption_style: Option<&str>, sub_style: Option<&SubStyle>) -> String {
    set_fonts_dir(fonts_dir());
    let subs = vec![
        Sub { start: 0.0, end: 2.0, tgt: "one two three".into(), y: Some(1500) },
        Sub { start: 2.0, end: 4.0, tgt: "four five six seven".into(), y: Some(1500) },
    ];
    let out = std::env::temp_dir().join(format!("dc_test_{}.ass", caption_style.unwrap_or("match")));
    let args = BuildArgs {
        subs: &subs,
        sub_style,
        sub_y: Some(1500),
        caption_style,
        ..Default::default()
    };
    build(1080, 1920, &out, args).unwrap();
    std::fs::read_to_string(&out).unwrap()
}

#[test]
fn head_has_core_styles() {
    let ass = gen(None, Some(&SubStyle::default()));
    assert!(ass.contains("[Script Info]"), "нет Script Info");
    assert!(ass.contains("PlayResX: 1080") && ass.contains("PlayResY: 1920"), "нет PlayRes");
    assert!(ass.contains("Style: S,"), "нет S-стиля");
    assert!(ass.contains("Style: T,"), "нет T-стиля (титры)");
    assert!(ass.contains("Style: KP,") && ass.contains("Style: KT,"), "нет KP/KT стилей");
    assert!(ass.contains("[Events]"), "нет секции Events");
}

#[test]
fn match_original_uses_s_style_events() {
    // без caption_style (match-original) субтитры идут стилем S на Layer 1.
    let ass = gen(None, Some(&SubStyle::default()));
    assert!(ass.contains("Dialogue: 1,") && ass.contains(",S,,"), "нет S-событий субтитра");
    // match-original НЕ должен эмитить karaoke/pop-теги.
    assert!(!ass.contains("\\kf"), "match-original не должен содержать \\kf");
}

#[test]
fn karaoke_preset_emits_kf() {
    let ass = gen(Some("karaoke"), None);
    assert!(ass.contains("\\kf"), "karaoke-пресет должен эмитить \\kf");
    assert!(ass.contains(",KT,,"), "стилизованный текст идёт стилем KT");
    assert!(ass.contains(",KP,,"), "плашка идёт стилем KP");
}

#[test]
fn word_and_pop_emit_alpha_transition() {
    let word = gen(Some("candy"), None); // candy: reveal=word
    assert!(word.contains("\\alpha&HFF&") && word.contains("\\t("), "word reveal: alpha+transition");
    let pop = gen(Some("mrbeast"), None); // mrbeast: reveal=pop
    assert!(pop.contains("\\fscx55") || pop.contains("\\fscy55"), "pop reveal: scale-in");
}

#[test]
fn hormozi_highlight_recolours_active_word() {
    let ass = gen(Some("hormozi"), None); // reveal=highlight, accent #FFD400
    // highlight перекрашивает активное слово в accent, потом обратно в base -> два \1c подряд в одном событии.
    assert!(ass.matches("\\1c").count() >= 3, "highlight должен многократно менять \\1c");
    assert!(ass.contains(",KT,,"), "highlight-текст идёт стилем KT");
}

#[test]
fn neon_uses_glow_plate() {
    let ass = gen(Some("neon"), None); // plate=glow
    // glow-плашка = размытый accent-ореол \blur за непрозрачной плашкой -> есть \blur в KP-событии.
    assert!(ass.contains("\\blur"), "glow-плашка должна содержать \\blur");
    assert!(ass.contains(",KP,,"), "плашка стилем KP");
}

#[test]
fn subtitles_are_non_overlapping() {
    // каждая строка кончается там, где начинается следующая: два субтитра 0-2 и 2-4 не пересекаются.
    let ass = gen(None, Some(&SubStyle::default()));
    // проверим что есть событие, кончающееся на 0:00:02.00 (конец первого == старт второго).
    assert!(ass.contains("0:00:02.00"), "границы окон должны совпадать (2.00)");
}

#[test]
fn uppercase_style_uppercases_text() {
    let mut ss = SubStyle::default();
    ss.uppercase = true;
    let ass = gen(None, Some(&ss));
    assert!(ass.contains("ONE TWO THREE"), "uppercase-стиль должен дать капс");
}
