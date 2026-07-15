//! Порт dubengine/translate.py — плоский MT через Gemma (llama.cpp): весь транскрипт как нумерованные
//! строки в ОДНОМ вызове (чанки по 40), чтобы каждая строка переводилась в контексте всего диалога;
//! глоссарий пиннит повторяющиеся ИМЕНА; при рассинхроне нумерации — надёжный per-line фолбэк.
//!
//! Промпты и параметры сэмплинга перенесены ДОСЛОВНО (они выверены на тест-сете). Не менять формулировки.

use std::collections::HashMap;

use dub_llm::{strip_think, ChatClient, Message, Sampling};
use regex::Regex;

use crate::seg::Seg;
use crate::TranslateError;

const CHUNK: usize = 40;

/// _LANGS из translate.py — код -> английское имя языка.
fn lang_name(code: &str, default: &str) -> String {
    let c = code.trim().to_lowercase();
    if c.is_empty() || c == "auto" {
        return default.to_string();
    }
    crate::WHISPER_LANGS
        .iter()
        .find(|(k, _)| *k == c.as_str())
        .map(|(_, v)| v.to_string())
        .unwrap_or_else(|| code.to_string())
}

/// _name(code, default="the source language").
fn name_src(code: &str) -> String {
    lang_name(code, "the source language")
}

fn has_cjk(s: &str) -> bool {
    // re.search(r"[぀-ヿ一-鿿]") — хирагана/катакана + CJK-иероглифы.
    s.chars().any(|c| ('\u{3040}'..='\u{30FF}').contains(&c) || ('\u{4E00}'..='\u{9FFF}').contains(&c))
}

/// _parse_numbered: вытащить строки 'N. перевод' (1..n) в порядке; None для пропущенных.
fn parse_numbered(text: &str, n: usize) -> Vec<Option<String>> {
    // re.match(r"\s*(\d+)\s*[.)\]:]\s*(.+)")
    let re = Regex::new(r"^\s*(\d+)\s*[.)\]:]\s*(.+)").unwrap();
    let mut got: HashMap<usize, String> = HashMap::new();
    for line in text.lines() {
        if let Some(c) = re.captures(line) {
            let i: usize = c[1].parse().unwrap_or(0);
            let val = c[2].trim();
            if (1..=n).contains(&i) && !got.contains_key(&i) && !val.is_empty() {
                // " ".join(m.group(2).split()) — схлопнуть пробелы.
                got.insert(i, val.split_whitespace().collect::<Vec<_>>().join(" "));
            }
        }
    }
    (1..=n).map(|i| got.get(&i).cloned()).collect()
}

/// _translate_one — нативный однострочный вызов (надёжный фолбэк при рассинхроне батча).
fn translate_one(
    llm: &ChatClient,
    txt: &str,
    tgt_name: &str,
    extra: &str,
    gloss_str: &str,
) -> Result<String, TranslateError> {
    let prompt = format!(
        "Translate the following text into {tgt_name}.{extra}{gloss_str} Note that you should only \
         output the translated result without any additional explanation:\n\n{txt}"
    );
    let s = Sampling::new(0.7, 0.6, 512).top_k(20).repeat_penalty(1.05);
    let out = strip_think(&llm.chat(&[Message::user_text(prompt)], &s)?);
    Ok(out
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" "))
}

/// _glossary — запиннить повторяющиеся собственные ИМЕНА (заглавные + повторяющиеся) для консистентности.
fn glossary<'a>(
    llm: &ChatClient,
    texts: impl Iterator<Item = &'a str>,
    src: &str,
    tgt: &str,
) -> Result<String, TranslateError> {
    // counts по \b[A-Z][a-z]{2,}\b
    let re = Regex::new(r"\b[A-Z][a-z]{2,}\b").unwrap();
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut order: Vec<String> = Vec::new(); // порядок ПЕРВОГО появления (Counter сохраняет вставку)
    for t in texts {
        for m in re.find_iter(t) {
            let w = m.as_str().to_string();
            let e = counts.entry(w.clone()).or_insert(0);
            if *e == 0 {
                order.push(w);
            }
            *e += 1;
        }
    }
    // most_common(6), c>=3 — по счёту убыв.; ничья -> порядок появления (стабильная сортировка), НЕ алфавит
    let mut items: Vec<(String, usize)> = order.iter().map(|w| (w.clone(), counts[w])).collect();
    items.sort_by(|a, b| b.1.cmp(&a.1));
    let terms: Vec<String> = items.into_iter().take(6).filter(|(_, c)| *c >= 3).map(|(w, _)| w).collect();

    let mut gloss: Vec<(String, String)> = Vec::new();
    for w in terms {
        let sys = format!(
            "Translate this single name/word from {src} to {tgt}. Output only the {tgt} word."
        );
        let s = Sampling::new(0.2, 0.9, 16);
        let v = strip_think(&llm.chat(&[Message::system(sys), Message::user_text(&w)], &s)?);
        // v.splitlines()[0].strip(' ."')
        let v = v.lines().next().unwrap_or("").trim_matches(|c| c == ' ' || c == '.' || c == '"').to_string();
        if !v.is_empty() && !has_cjk(&v) {
            gloss.push((w, v));
        }
    }
    if gloss.is_empty() {
        Ok(String::new())
    } else {
        let joined = gloss.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(", ");
        Ok(format!(" Keep these names consistent: {joined}."))
    }
}

/// Индексы непустых сегментов + число уникальных спикеров среди них (общий шаг run/rewrite).
fn nonempty_idxs_and_nspk(segs: &[Seg]) -> (Vec<usize>, usize) {
    let idxs: Vec<usize> =
        segs.iter().enumerate().filter(|(_, s)| !s.text.trim().is_empty()).map(|(i, _)| i).collect();
    let nspk = idxs
        .iter()
        .map(|&i| segs[i].speaker)
        .collect::<std::collections::HashSet<i64>>()
        .len();
    (idxs, nspk)
}

/// Нумерованный блок "1. текст\n2. текст…" для чанка индексов (общий для run/rewrite).
fn numbered_block(segs: &[Seg], chunk: &[usize]) -> String {
    chunk
        .iter()
        .enumerate()
        .map(|(j, &gi)| format!("{}. {}", j + 1, segs[gi].text.trim()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// run — перевод каждого seg.text -> seg.tgt через Gemma (плоский MT, порт _run_hunyuan).
pub fn run(
    llm: &ChatClient,
    segs: &mut [Seg],
    src: &str,
    tgt: &str,
    spoken: bool,
) -> Result<(), TranslateError> {
    let tgt_name = lang_name(tgt, tgt);
    let gloss_str = glossary(llm, segs.iter().map(|s| s.text.as_str()), &name_src(src), &tgt_name)?;
    let extra = if spoken {
        " Spell out all numbers, dates, times and symbols as full words."
    } else {
        ""
    };
    for s in segs.iter_mut() {
        s.tgt = String::new();
    }
    let (idxs, nspk) = nonempty_idxs_and_nspk(segs);

    let mut c0 = 0;
    while c0 < idxs.len() {
        let chunk = &idxs[c0..(c0 + CHUNK).min(idxs.len())];
        let numbered = numbered_block(segs, chunk);
        let dlg = if nspk > 1 {
            format!(
                " This is a DIALOGUE between {nspk} speakers taking turns — render it as one coherent \
                 back-and-forth conversation, keeping each speaker's voice and tone consistent."
            )
        } else {
            String::new()
        };
        let sysmsg = format!(
            "You are a professional subtitle translator localizing a SHORT video for DUBBING into {tgt_name}.\
             {dlg} Use the WHOLE numbered list as shared context so each line (even one word) is correct and \
             consistent. Preserve the MEANING, write natural SPOKEN {tgt_name}, and keep each line about the \
             SAME LENGTH as its source so it fits the dub timing.{extra}{gloss_str} Reply with ONLY the \
             numbered {tgt_name} translations (1., 2., 3., …), one per line, nothing else — no reasoning, no \
             English, no notes."
        );
        let max_tokens = (96 + 48 * chunk.len()).min(4096) as u32;
        let s = Sampling::new(0.3, 0.9, max_tokens).top_k(20).repeat_penalty(1.05);
        let out = strip_think(&llm.chat(&[Message::system(sysmsg), Message::user_text(numbered)], &s)?);
        let parsed = parse_numbered(&out, chunk.len());
        if parsed.iter().all(|p| p.is_some()) {
            for (&gi, p) in chunk.iter().zip(parsed) {
                segs[gi].tgt = p.unwrap();
            }
        } else {
            // нумерация уплыла -> надёжный per-line режим для этого чанка.
            for &gi in chunk {
                let txt = segs[gi].text.trim().to_string();
                segs[gi].tgt = translate_one(llm, &txt, &tgt_name, extra, &gloss_str)?;
            }
        }
        c0 += CHUNK;
    }
    // деградация: пустые -> оставить исходник, чтобы дубляж не был пуст (как в питоне).
    let empty: Vec<usize> = idxs.iter().cloned().filter(|&gi| segs[gi].tgt.is_empty()).collect();
    for &gi in &empty {
        segs[gi].tgt = segs[gi].text.trim().to_string();
    }
    if !idxs.is_empty() && empty.len() == idxs.len() {
        return Err(TranslateError::Empty(idxs.len()));
    }
    Ok(())
}

/// rewrite — творческое ПЕРЕОЗВУЧИВАНИЕ всего транскрипта по инструкции. Порт translate.rewrite.
pub fn rewrite(
    llm: &ChatClient,
    segs: &mut [Seg],
    instruction: &str,
    _src: &str,
    tgt: &str,
    spoken: bool,
) -> Result<(), TranslateError> {
    let tgt_name = lang_name(tgt, tgt);
    for s in segs.iter_mut() {
        s.tgt = String::new();
    }
    let (idxs, nspk) = nonempty_idxs_and_nspk(segs);
    let extra = if spoken {
        " Spell out all numbers, dates, times and symbols as full words."
    } else {
        ""
    };
    let mut c0 = 0;
    while c0 < idxs.len() {
        let chunk = &idxs[c0..(c0 + CHUNK).min(idxs.len())];
        let numbered = numbered_block(segs, chunk);
        let dlg = if nspk > 1 {
            format!(" It is a dialogue between {nspk} speakers taking turns — keep the back-and-forth.")
        } else {
            String::new()
        };
        // То же, что ctx.rs: ЗАМЕНИТЬ содержимое на тему/стиль инструкции, НЕ переводить исходник (иначе Q4
        // просто переводит, тема не меняется — репорт юзера). Оба пути (funny-анализ и editor-remix) одинаковы.
        let sysmsg = format!(
            "You are a creative scriptwriter writing a BRAND-NEW voice-over script in {tgt_name} for a short video.{dlg} \
             IGNORE the literal meaning of the source lines — they are ONLY a rhythm/length template. Write a completely \
             NEW script whose CONTENT follows this instruction: \"{instruction}\". Every line must fit the instruction, \
             NOT translate the source. Keep the SAME number of lines and make each new line roughly the SAME LENGTH as \
             its source line so it fits the dub timing.{extra} Output natural spoken {tgt_name}. Reply with ONLY the \
             numbered {tgt_name} lines (1., 2., 3., …), nothing else — no notes, no source text."
        );
        let max_tokens = (128 + 64 * chunk.len()).min(4096) as u32;
        let s = Sampling::new(0.85, 0.95, max_tokens).top_k(40).repeat_penalty(1.05);
        let out = strip_think(&llm.chat(&[Message::system(sysmsg), Message::user_text(numbered)], &s)?);
        let parsed = parse_numbered(&out, chunk.len());
        for (j, &gi) in chunk.iter().enumerate() {
            let src_line = segs[gi].text.trim().to_string();
            if let Some(p) = &parsed[j] {
                segs[gi].tgt = p.clone();
            } else {
                // пропущенная/сбитая строка -> перевести её (не озвучивать сырой исходник).
                let one = translate_one(llm, &src_line, &tgt_name, extra, "")?;
                segs[gi].tgt = if one.is_empty() { src_line } else { one };
            }
        }
        c0 += CHUNK;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_numbered_basic() {
        let out = "1. Hello\n2. World\n3) Foo";
        let p = parse_numbered(out, 3);
        assert_eq!(p[0].as_deref(), Some("Hello"));
        assert_eq!(p[1].as_deref(), Some("World"));
        assert_eq!(p[2].as_deref(), Some("Foo"));
    }

    #[test]
    fn parse_numbered_missing() {
        let p = parse_numbered("1. A\n3. C", 3);
        assert_eq!(p[0].as_deref(), Some("A"));
        assert!(p[1].is_none());
        assert_eq!(p[2].as_deref(), Some("C"));
    }

    #[test]
    fn lang_names() {
        assert_eq!(lang_name("ru", "x"), "Russian");
        assert_eq!(name_src("auto"), "the source language");
        assert_eq!(lang_name("xx", "fallback"), "xx");
    }

    #[test]
    fn cjk_detect() {
        assert!(has_cjk("こんにちは"));
        assert!(has_cjk("中文"));
        assert!(!has_cjk("Hello"));
    }
}
