//! OLED text helpers: ASCII folding and Japanese romanisation.

use std::sync::OnceLock;
use unicode_normalization::UnicodeNormalization;

/// The OLED has no accents: "Música" becomes "Musica".
pub fn ascii_only(text: &str) -> String {
    let folded: String = text
        .nfkd()
        .filter(|c| c.is_ascii())
        .collect::<String>()
        .replace('\n', " ");
    folded.trim().chars().take(60).collect()
}

fn is_japanese(c: char) -> bool {
    let o = c as u32;
    (0x3040..=0x30FF).contains(&o)
        || (0x3400..=0x4DBF).contains(&o)
        || (0x4E00..=0x9FFF).contains(&o)
        || o == 0x30FC
}

fn has_kanji(text: &str) -> bool {
    text.chars().any(|c| {
        let o = c as u32;
        (0x3400..=0x4DBF).contains(&o) || (0x4E00..=0x9FFF).contains(&o)
    })
}

fn only_kana(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| (0x3040..=0x30FF).contains(&(c as u32)) || c == 'ー')
}

const PARTICLES: &[&str] = &[
    "に", "の", "は", "を", "が", "で", "と", "へ", "も", "や", "か", "な", "ね", "よ", "だ",
    "です", "ます", "から", "まで", "より", "って", "だけ", "しか", "ほど", "ばかり", "こそ",
    "さえ", "でも", "では", "には", "とは", "への", "からは",
];

static ROMANIZER: OnceLock<Option<ib_romaji::HepburnRomanizer>> = OnceLock::new();

fn romanizer() -> Option<&'static ib_romaji::HepburnRomanizer> {
    ROMANIZER
        .get_or_init(|| Some(ib_romaji::HepburnRomanizer::default()))
        .as_ref()
}

/// Load the dictionary ahead of time so the first track does not stall.
pub fn warm_romanizer() {
    let _ = romanizer();
}

fn push_token(out: &mut String, token: &str, is_word: bool) {
    if token.is_empty() {
        return;
    }
    if !out.is_empty() && !is_word {
        // Literal run: keep appending so Latin words are not split.
        out.push_str(token);
        return;
    }
    if !out.is_empty() {
        let last = out.chars().last().unwrap_or(' ');
        let first = token.chars().next().unwrap_or(' ');
        if last.is_alphanumeric() && first.is_alphanumeric() {
            out.push(' ');
        }
    }
    out.push_str(token);
}

fn capitalise(text: String) -> String {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return text;
    };
    if first.is_lowercase() {
        let mut out = first.to_uppercase().collect::<String>();
        out.extend(chars);
        out
    } else {
        text
    }
}

/// Japanese -> romaji for the OLED. Latin text passes through unchanged.
pub fn romanize(text: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        return String::new();
    }
    if !text.chars().any(is_japanese) {
        return ascii_only(text);
    }
    let Some(romanizer) = romanizer() else {
        return ascii_only(text);
    };

    let mut out = String::new();
    let mut pos = 0usize;
    let mut previous: Option<(String, String)> = None;

    while pos < text.len() {
        let rest = &text[pos..];
        let mut found: Option<(usize, &'static str)> = None;
        romanizer.romanize_and_try_for_each(rest, |len, romaji| {
            found = Some((len, romaji));
            Some(())
        });

        if let Some((len, romaji)) = found {
            let source = rest[..len].to_string();
            let merge = previous
                .as_ref()
                .map(|(prev_source, _)| {
                    (only_kana(&source)
                        && !PARTICLES.contains(&source.as_str())
                        && has_kanji(prev_source))
                        || (has_kanji(&source)
                            && has_kanji(prev_source)
                            && source.chars().count() <= 2)
                })
                .unwrap_or(false);
            if merge {
                out.push_str(romaji);
            } else {
                push_token(&mut out, romaji, true);
            }
            previous = Some((source, romaji.to_string()));
            pos += len;
        } else {
            let ch = rest.chars().next().unwrap();
            if ch.is_ascii() {
                push_token(&mut out, &ch.to_string(), false);
            } else if ch.is_whitespace() || !is_japanese(ch) {
                previous = None;
                if ch.is_whitespace() {
                    out.push(' ');
                }
            }
            pos += ch.len_utf8();
        }
    }

    let joined = out.split_whitespace().collect::<Vec<_>>().join(" ");
    ascii_only(&capitalise(joined))
}
