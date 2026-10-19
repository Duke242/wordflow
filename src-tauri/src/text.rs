//! Post-processing applied to raw transcripts before they are pasted.
//!
//! Ported from the macOS app so both produce the same text:
//! `WhisperService.normalizedTranscription`, `DictionaryService.apply` and
//! `SmartTrailingPunctuation`. The tests at the bottom mirror the Swift tests.

use std::sync::LazyLock;

use regex::{NoExpand, Regex};
use serde::{Deserialize, Serialize};

use crate::{snippets::Snippet, spoken, tone::AppCategory};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DictionaryEntry {
    pub id: String,
    pub trigger: String,
    pub replacement: String,
    #[serde(default = "yes")]
    pub is_enabled: bool,
    #[serde(default = "yes")]
    pub match_whole_word: bool,
    /// Added by `learn` from a correction, rather than by the user.
    #[serde(default)]
    pub learned: bool,
    /// The kinds of app the entry is used in, like code editors and terminals
    /// for "emerge" to "merge". Empty for every app.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "app_categories"
    )]
    pub only_in: Vec<AppCategory>,
}

/// Kinds of app as saved, reading one this version doesn't know, from a newer
/// version or another device, as "other" rather than failing to load the
/// settings it's in. Apps of that kind are sorted into "other" here too.
pub fn app_categories<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<AppCategory>, D::Error> {
    let values = Vec::<serde_json::Value>::deserialize(deserializer)?;
    let mut categories = Vec::new();
    for value in values {
        let category = serde_json::from_value(value).unwrap_or(AppCategory::Other);
        if !categories.contains(&category) {
            categories.push(category);
        }
    }
    Ok(categories)
}

impl DictionaryEntry {
    /// Whether the entry is used when dictating into this kind of app.
    pub fn applies_in(&self, app: AppCategory) -> bool {
        self.is_enabled && (self.only_in.is_empty() || self.only_in.contains(&app))
    }
}

fn yes() -> bool {
    true
}

pub struct Options<'a> {
    pub auto_edit: bool,
    pub smart_trailing_punctuation: bool,
    /// Commas instead of the semicolons and colons the engine adds.
    pub plain_punctuation: bool,
    pub dictionary: &'a [DictionaryEntry],
    /// The kind of app being dictated into, which picks the dictionary
    /// entries that apply.
    pub app: AppCategory,
    /// "Scratch that", "new line" and "new paragraph".
    pub voice_commands: bool,
    /// "Meet at 3, actually 4" becomes "meet at 4".
    pub self_corrections: bool,
    /// Spoken lists, numbers, dates and times written out.
    pub smart_formatting: bool,
    pub snippets: &'a [Snippet],
    /// The three rules above only understand English.
    pub english: bool,
}

/// Whether the spoken language setting allows English: chosen, or detected.
pub fn is_english(language: &str) -> bool {
    language == "auto" || language == "en" || language.starts_with("en-")
}

/// Full pipeline: engine cleanup, then dictionary, then trailing punctuation.
/// Dictation runs the two halves separately, with optional language-model
/// cleanup in between.
#[cfg(test)]
pub fn process(raw: &str, options: &Options) -> String {
    finish(
        &normalize_transcription(raw, options.auto_edit, options.plain_punctuation),
        options,
    )
}

/// Voice commands, corrections and formatting, then dictionary rules,
/// snippets and trailing punctuation.
pub fn finish(text: &str, options: &Options) -> String {
    let english = |on: bool| on && options.english;
    let mut text = text.to_string();
    if english(options.voice_commands) {
        text = spoken::apply_commands(&text);
    }
    if english(options.self_corrections) {
        text = spoken::apply_corrections(&text);
    }
    let format = english(options.smart_formatting);
    if format {
        text = spoken::format(&text);
    }
    let entries = options
        .dictionary
        .iter()
        .filter(|e| e.applies_in(options.app));
    let text = apply_dictionary(&text, entries);
    let text = crate::snippets::expand(&text, options.snippets, |trigger| {
        if format {
            spoken::format(trigger)
        } else {
            trigger.to_string()
        }
    });
    if options.smart_trailing_punctuation {
        strip_trailing_period(&text)
    } else {
        text
    }
}

/// The patterns below are fixed at compile time and exercised by the tests, so
/// failing to compile one is a programming error.
const STATIC_PATTERN: &str = "static regex pattern is valid";

static PLACEHOLDERS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"\[(?:BLANK_AUDIO|SILENCE)\]",
        r"<\|nospeech\|>",
        r"\[\s*S\s*\]",
    ]
    .iter()
    .map(|p| Regex::new(p).expect(STATIC_PATTERN))
    .collect()
});

const NOISE_TERMS: &[&str] = &[
    "applause",
    "background noise",
    "blank audio",
    "breathing",
    "cough",
    "coughing",
    "exhale",
    "heartbeat",
    "indistinct",
    "inaudible",
    "inhale",
    "laughing",
    "laughter",
    "loud noise",
    "muffled speech",
    "music",
    "noise",
    "silence",
    "sigh",
    "sighs",
    "sniffing",
    "static",
    "unclear speech",
    "unintelligible",
    "wind",
    "wind blowing",
    "wind noise",
];

static NOISE_LABEL: LazyLock<Regex> = LazyLock::new(|| {
    let terms: Vec<String> = NOISE_TERMS.iter().map(|t| regex::escape(t)).collect();
    Regex::new(&format!(r"(?i)[\[(]\s*(?:{})\s*[\])]", terms.join("|"))).expect(STATIC_PATTERN)
});

static WHITESPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect(STATIC_PATTERN));

/// Hesitation sounds that Auto Edit removes.
const FILLERS: &str = r"uh+|um+|umm+|uhm+|erm+|hmm+";

// The Swift pattern ends in a lookahead, `(?=$|[\s,.;:!?])`, which the regex crate
// doesn't support. `remove_fillers` checks that boundary by hand instead.
static FILLER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"(?i)(^|[\s,.;:!?])(?:{FILLERS})")).expect(STATIC_PATTERN)
});

static FILLER_WORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"(?i)^(?:{FILLERS})$")).expect(STATIC_PATTERN));

/// Whether `word`, without punctuation, is a hesitation sound like "um".
pub fn is_filler(word: &str) -> bool {
    FILLER_WORD.is_match(word)
}

/// A semicolon or colon the engine put between words. People rarely type them
/// in messages, so they become commas. Times ("3:30") and links ("https://")
/// have no space after the colon and are left alone.
static SEMICOLON_OR_COLON: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\w)\s*[;:]\s+").expect(STATIC_PATTERN));

static SPACE_BEFORE_PUNCTUATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s+([,.;:!?])").expect(STATIC_PATTERN));

/// Removes Whisper's placeholder tokens and bracketed noise labels, and
/// optionally filler words ("Auto Edit") and semicolons and colons ("plain
/// punctuation"). Spoken lists put back the colon that introduces them.
pub fn normalize_transcription(text: &str, auto_edit: bool, plain_punctuation: bool) -> String {
    let mut out = text.to_string();
    for re in PLACEHOLDERS.iter() {
        out = re.replace_all(&out, " ").into_owned();
    }
    out = NOISE_LABEL.replace_all(&out, " ").into_owned();
    out = WHITESPACE.replace_all(&out, " ").into_owned();
    if plain_punctuation {
        out = SEMICOLON_OR_COLON.replace_all(&out, "${1}, ").into_owned();
    }

    if auto_edit {
        out = remove_fillers(&out);
        out = SPACE_BEFORE_PUNCTUATION
            .replace_all(&out, "$1")
            .into_owned();
        out = WHITESPACE.replace_all(&out, " ").into_owned();
    }
    out.trim().to_string()
}

fn is_filler_boundary(c: char) -> bool {
    c.is_whitespace() || ",.;:!?".contains(c)
}

fn remove_fillers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut copied_to = 0;
    let mut search_from = 0;

    while let Some(caps) = FILLER.captures_at(text, search_from) {
        // Group 0 is the whole match and group 1 always participates (it can
        // match the empty `^`), so both are present on every match.
        let (Some(whole), Some(boundary)) = (caps.get(0), caps.get(1)) else {
            break;
        };
        let rest = &text[whole.end()..];
        let mut after = rest.chars();
        let boundary_ok = match after.next() {
            None => true,
            Some(c) => is_filler_boundary(c),
        };

        if !boundary_ok {
            // Retry one character further on, as a backtracking engine would.
            match text[whole.start()..].chars().next() {
                Some(c) => search_from = whole.start() + c.len_utf8(),
                None => break,
            }
            continue;
        }

        // Keep the boundary character before the filler, drop the filler and at
        // most one punctuation mark straight after it.
        let mut end = whole.end();
        if let Some(c) = rest.chars().next()
            && ",.;:!?".contains(c)
        {
            end += c.len_utf8();
        }
        out.push_str(&text[copied_to..whole.start()]);
        out.push_str(boundary.as_str());
        copied_to = end;
        search_from = end;
        if search_from >= text.len() {
            break;
        }
    }
    out.push_str(&text[copied_to..]);
    out
}

/// Applies enabled dictionary entries in order. Each entry sees the output of
/// the ones before it.
pub fn apply_dictionary<'a>(
    text: &str,
    entries: impl IntoIterator<Item = &'a DictionaryEntry>,
) -> String {
    let mut out = text.to_string();
    if text.is_empty() {
        return out;
    }
    for entry in entries.into_iter().filter(|e| e.is_enabled) {
        let trigger = entry.trigger.trim();
        if trigger.is_empty() {
            continue;
        }
        let Some(re) = dictionary_regex(trigger, entry.match_whole_word) else {
            continue;
        };
        out = re
            .replace_all(&out, NoExpand(&entry.replacement))
            .into_owned();
    }
    out
}

fn dictionary_regex(trigger: &str, whole_word: bool) -> Option<Regex> {
    let mut pattern = regex::escape(trigger).replace(' ', r"\s+");
    let is_word_char = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
    if whole_word && is_word_char(trigger.chars().next()) {
        pattern = format!(r"\b{pattern}");
    }
    if whole_word && is_word_char(trigger.chars().last()) {
        pattern = format!(r"{pattern}\b");
    }
    Regex::new(&format!("(?i){pattern}")).ok()
}

/// Most words hinted in a prompt, and the most characters of each part. Together
/// they stay well inside the 224 tokens Whisper reads.
const MAX_HINTS: usize = 40;
const MAX_HINT_CHARS: usize = 400;
const MAX_CONTEXT_CHARS: usize = 300;

/// Text for Whisper to read before the audio: the words the dictionary
/// corrects to, so names and jargon are expected rather than guessed, then the
/// end of what was already said in this dictation, if anything. Only entries
/// used in `app` are hinted.
pub fn prompt(dictionary: &[DictionaryEntry], previous: &str, app: AppCategory) -> String {
    let mut hints: Vec<&str> = Vec::new();
    let mut chars = 0;
    // Newest entries first, so recently learned words are kept when there are many.
    for entry in dictionary.iter().rev().filter(|e| e.applies_in(app)) {
        let word = entry.replacement.trim();
        if !is_vocabulary(word) || hints.iter().any(|h| h.eq_ignore_ascii_case(word)) {
            continue;
        }
        if hints.len() == MAX_HINTS || chars + word.len() + 2 > MAX_HINT_CHARS {
            break;
        }
        chars += word.len() + 2;
        hints.push(word);
    }
    let mut prompt = hints.join(", ");
    if !prompt.is_empty() {
        prompt.push('.');
    }
    let context = tail(previous.trim(), MAX_CONTEXT_CHARS);
    if !context.is_empty() {
        if !prompt.is_empty() {
            prompt.push(' ');
        }
        prompt.push_str(context);
    }
    prompt.replace('\0', "")
}

/// A word or short name worth hinting. Snippets (an address for "my email") and
/// filler removal (an empty replacement) aren't words Whisper should expect.
fn is_vocabulary(text: &str) -> bool {
    !text.is_empty()
        && text.split_whitespace().count() <= 3
        && text.chars().count() <= 32
        && !text.contains(['@', '/', '\n'])
        && text.chars().any(char::is_alphabetic)
}

/// The last `max` characters of `text`, starting at a word.
fn tail(text: &str, max: usize) -> &str {
    let Some((start, _)) = text.char_indices().rev().nth(max) else {
        return text;
    };
    let rest = &text[start..];
    match rest.find(char::is_whitespace) {
        Some(space) => rest[space..].trim_start(),
        None => "",
    }
}

static NUMBER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^\+?\(?\d(?:[\d\s.,\-()/:]*\d)?$").expect(STATIC_PATTERN));
static EMAIL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^[^\s@]+@[^\s@]+\.[^\s@]+$").expect(STATIC_PATTERN));
static URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(?:[a-z][a-z0-9+.\-]*://\S+|www\.\S+\.\S+|[a-z0-9\-]+(?:\.[a-z0-9\-]+)*\.[a-z]{2,}(?:[/:?#]\S*)?)$",
    )
    .expect(STATIC_PATTERN)
});

/// Drops the period Whisper adds after short non-prose dictations such as an
/// email address, URL, number or single word. Sentences keep their period.
pub fn strip_trailing_period(text: &str) -> String {
    let Some(candidate) = text.trim().strip_suffix('.') else {
        return text.to_string();
    };
    if candidate.is_empty() || candidate.ends_with('.') {
        return text.to_string();
    }
    let is_plain_token = !candidate.chars().any(|c| c.is_whitespace() || c == '.');
    if NUMBER.is_match(candidate)
        || EMAIL.is_match(candidate)
        || URL.is_match(candidate)
        || is_plain_token
    {
        candidate.to_string()
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(trigger: &str, replacement: &str, whole_word: bool) -> DictionaryEntry {
        DictionaryEntry {
            id: trigger.to_string(),
            trigger: trigger.to_string(),
            replacement: replacement.to_string(),
            is_enabled: true,
            match_whole_word: whole_word,
            learned: false,
            only_in: Vec::new(),
        }
    }

    #[test]
    fn prompt_hints_words_and_ends_with_what_was_said() {
        let dictionary = [
            entry("karen", "Karan", true),
            entry("cooper netties", "Kubernetes", true),
            entry("my email", "karan@example.com", true),
            entry("um", "", false),
            entry("cube our netties", "Kubernetes", true),
        ];
        assert_eq!(
            prompt(&dictionary, "", AppCategory::Other),
            "Kubernetes, Karan."
        );
        assert_eq!(
            prompt(&dictionary, "  so we deployed it ", AppCategory::Other),
            "Kubernetes, Karan. so we deployed it"
        );
        assert_eq!(
            prompt(&[], "hello there", AppCategory::Other),
            "hello there"
        );
        assert_eq!(prompt(&[], "", AppCategory::Other), "");

        let mut disabled = entry("teh", "the", true);
        disabled.is_enabled = false;
        assert_eq!(prompt(&[disabled], "", AppCategory::Other), "");
    }

    #[test]
    fn entries_limited_to_some_apps_only_apply_there() {
        let mut merge = entry("emerge", "merge", true);
        merge.only_in = vec![AppCategory::Code, AppCategory::Terminal];
        let dictionary = [merge, entry("word flow", "WordFlow", true)];
        let in_app = |app| Options {
            app,
            ..options(&dictionary, &[])
        };
        assert_eq!(
            process("Emerge it into word flow", &in_app(AppCategory::Code)),
            "merge it into WordFlow"
        );
        assert_eq!(
            process("Emerge it into word flow", &in_app(AppCategory::Terminal)),
            "merge it into WordFlow"
        );
        assert_eq!(
            process("Emerge it into word flow", &in_app(AppCategory::Chat)),
            "Emerge it into WordFlow"
        );
        assert_eq!(
            prompt(&dictionary, "", AppCategory::Code),
            "WordFlow, merge."
        );
        assert_eq!(prompt(&dictionary, "", AppCategory::Email), "WordFlow.");
    }

    #[test]
    fn entries_saved_without_apps_apply_everywhere() {
        let saved = r#"{"id":"1","trigger":"apr","replacement":"a PR"}"#;
        let entry: DictionaryEntry = serde_json::from_str(saved).expect("old entry loads");
        assert!(entry.only_in.is_empty());
        assert!(entry.applies_in(AppCategory::Chat));
        // Left out when empty, so entries read the same in older versions.
        let json = serde_json::to_string(&entry).expect("serializes");
        assert!(!json.contains("onlyIn"), "{json}");
    }

    #[test]
    fn unknown_kinds_of_app_load_as_other() {
        let saved =
            r#"{"id":"1","trigger":"a","replacement":"b","onlyIn":["code","someday","later"]}"#;
        let entry: DictionaryEntry = serde_json::from_str(saved).expect("entry still loads");
        assert_eq!(entry.only_in, [AppCategory::Code, AppCategory::Other]);
    }

    #[test]
    fn prompt_keeps_only_the_end_of_long_context_from_a_word_start() {
        let said = "word ".repeat(200);
        let prompt = prompt(&[], &said, AppCategory::Other);
        assert!(prompt.chars().count() <= MAX_CONTEXT_CHARS);
        assert!(prompt.starts_with("word") && prompt.ends_with("word"));
        assert_eq!(tail("héllo wörld", 5), "wörld");
        assert_eq!(tail("short", 50), "short");
    }

    #[test]
    fn prompt_hints_are_capped() {
        let many: Vec<_> = (0..100)
            .map(|i| entry(&format!("t{i}"), &format!("Name{i}"), true))
            .collect();
        let prompt = prompt(&many, "", AppCategory::Other);
        assert_eq!(prompt.split(", ").count(), MAX_HINTS);
        assert!(prompt.starts_with("Name99"), "newest first: {prompt}");
    }

    #[test]
    fn normalization_removes_whisper_placeholders() {
        assert_eq!(
            normalize_transcription(
                " [BLANK_AUDIO]  hello   <|nospeech|> [SILENCE] ",
                false,
                false
            ),
            "hello"
        );
        assert_eq!(
            normalize_transcription("[wind blowing] (heartbeat) answer [S]", false, false),
            "answer"
        );
        assert_eq!(
            normalize_transcription("[wind] (Loud noise) (indistinct)", false, false),
            ""
        );
    }

    #[test]
    fn placeholders_are_case_sensitive_but_noise_labels_are_not() {
        assert_eq!(
            normalize_transcription("[blank_audio] hi", false, false),
            "[blank_audio] hi"
        );
        assert_eq!(
            normalize_transcription("[MUSIC] hi (wind)", false, false),
            "hi"
        );
    }

    #[test]
    fn plain_punctuation_turns_semicolons_and_colons_into_commas() {
        assert_eq!(
            normalize_transcription("I went home; it was late.", false, true),
            "I went home, it was late."
        );
        assert_eq!(
            normalize_transcription("Here's the thing: we ship it ; then we wait.", false, true),
            "Here's the thing, we ship it, then we wait."
        );
        assert_eq!(
            normalize_transcription("Meet at 3:30 on https://zoom.us/j/1.", false, true),
            "Meet at 3:30 on https://zoom.us/j/1."
        );
        assert_eq!(
            normalize_transcription("Smile :) ok", false, true),
            "Smile :) ok"
        );
        assert_eq!(
            normalize_transcription("I went home; it was late.", false, false),
            "I went home; it was late."
        );
    }

    #[test]
    fn auto_edit_removes_fillers() {
        assert_eq!(normalize_transcription("um um", true, false), "");
        assert_eq!(
            normalize_transcription("So, um, I think uhh we should go.", true, false),
            "So, I think we should go."
        );
        assert_eq!(normalize_transcription("Hmm. Okay", true, false), "Okay");
        // Words that merely start with a filler are left alone.
        assert_eq!(
            normalize_transcription("umbrella hummus", true, false),
            "umbrella hummus"
        );
        assert_eq!(normalize_transcription("um, hm er", true, false), "hm er");
        // Auto Edit off keeps fillers.
        assert_eq!(
            normalize_transcription("um hello", false, false),
            "um hello"
        );
    }

    #[test]
    fn dictionary_matches_whole_words_case_insensitively() {
        let entries = [entry("word flow", "WordFlow", true)];
        assert_eq!(
            apply_dictionary("I use Word   Flow daily", &entries),
            "I use WordFlow daily"
        );
        let entries = [entry("cat", "dog", true)];
        assert_eq!(
            apply_dictionary("cat concatenate", &entries),
            "dog concatenate"
        );
        let entries = [entry("cat", "dog", false)];
        assert_eq!(
            apply_dictionary("cat concatenate", &entries),
            "dog condogenate"
        );
    }

    #[test]
    fn dictionary_skips_boundaries_for_symbols_and_inserts_literally() {
        let entries = [entry("@home", "$1 home", true)];
        assert_eq!(
            apply_dictionary("at x@home now", &entries),
            "at x$1 home now"
        );
    }

    #[test]
    fn dictionary_rules_chain_and_skip_disabled() {
        let mut disabled = entry("b", "z", true);
        disabled.is_enabled = false;
        let entries = [entry("a", "b", true), disabled, entry("b", "c", true)];
        assert_eq!(apply_dictionary("a", &entries), "c");
    }

    #[test]
    fn smart_trailing_punctuation_strips_non_prose() {
        let cases = [
            ("roy.sanhik@gmail.com.", "roy.sanhik@gmail.com"),
            (
                "https://example.com/pricing.",
                "https://example.com/pricing",
            ),
            ("www.example.com.", "www.example.com"),
            ("example.com.", "example.com"),
            ("example.com/docs/setup.", "example.com/docs/setup"),
            ("3.14.", "3.14"),
            ("1,000.", "1,000"),
            ("1.2.3.", "1.2.3"),
            ("+49 170 1234567.", "+49 170 1234567"),
            ("(555) 123-4567.", "(555) 123-4567"),
            ("5.", "5"),
            ("Hello.", "Hello"),
            ("ABC123.", "ABC123"),
            ("€1,000.", "€1,000"),
            (" roy.sanhik@gmail.com. ", "roy.sanhik@gmail.com"),
        ];
        for (input, expected) in cases {
            assert_eq!(strip_trailing_period(input), expected, "input: {input:?}");
        }
    }

    #[test]
    fn smart_trailing_punctuation_keeps_prose() {
        for input in [
            "This is a normal sentence.",
            "Email me at roy@gmail.com.",
            "Wait...",
            "Wait…",
            "U.S.",
            "Really?",
            "Stop!",
            ".",
            "",
            "roy@gmail.com",
        ] {
            assert_eq!(strip_trailing_period(input), input, "input: {input:?}");
        }
    }

    #[test]
    fn filler_removal_handles_unicode_neighbours() {
        // Accented letters after a filler make it part of a word.
        assert_eq!(normalize_transcription("umé ummé", true, false), "umé ummé");
        // Multi-byte text around a removed filler survives intact.
        assert_eq!(
            normalize_transcription("café, um, naïve 日本語 uh", true, false),
            "café, naïve 日本語"
        );
        // An ellipsis character isn't a boundary, so the filler stays.
        assert_eq!(
            normalize_transcription("um… right", true, false),
            "um… right"
        );
        assert_eq!(normalize_transcription("", true, false), "");
        assert_eq!(normalize_transcription("   ", true, false), "");
    }

    #[test]
    fn dictionary_handles_unicode_and_empty_input() {
        let entries = [entry("café", "coffee shop", true)];
        assert_eq!(
            apply_dictionary("Meet at the CAFÉ, not cafés", &entries),
            "Meet at the coffee shop, not cafés"
        );
        let entries = [entry("  ", "x", true), entry("ß", "ss", false)];
        assert_eq!(apply_dictionary("Straße", &entries), "Strasse");
        assert_eq!(apply_dictionary("", &entries), "");
    }

    #[test]
    fn smart_trailing_punctuation_handles_unicode() {
        assert_eq!(strip_trailing_period("東京."), "東京");
        assert_eq!(strip_trailing_period("naïve café."), "naïve café.");
        assert_eq!(strip_trailing_period("é"), "é");
    }

    fn options<'a>(dictionary: &'a [DictionaryEntry], snippets: &'a [Snippet]) -> Options<'a> {
        Options {
            auto_edit: true,
            smart_trailing_punctuation: true,
            plain_punctuation: true,
            dictionary,
            app: AppCategory::Other,
            voice_commands: true,
            self_corrections: true,
            smart_formatting: true,
            snippets,
            english: true,
        }
    }

    #[test]
    fn spoken_rules_run_in_order_before_dictionary_and_snippets() {
        let dictionary = [entry("word flow", "WordFlow", true)];
        let snippets = [Snippet {
            id: "1".into(),
            trigger: "my zoom room".into(),
            expansion: "https://zoom.us/j/1".into(),
            is_enabled: true,
        }];
        let options = options(&dictionary, &snippets);
        assert_eq!(
            process(
                "um, meet at three pm, actually four pm in my zoom room.",
                &options
            ),
            "meet at 4 PM in https://zoom.us/j/1."
        );
        assert_eq!(
            process(
                "Try word flow. Scratch that. Use word flow new line thanks",
                &options
            ),
            "Use WordFlow\nThanks"
        );
        assert_eq!(process("My zoom room.", &options), "https://zoom.us/j/1");
        assert_eq!(
            process(
                "todo: first, twenty five emails. Second, call Bob.",
                &options
            ),
            "todo:\n1. 25 emails\n2. Call Bob"
        );
    }

    #[test]
    fn everyday_sentences_pass_through_unchanged() {
        let options = options(&[], &[]);
        for said in [
            "I think we should ship it behind a flag first and watch the numbers.",
            "Let's move the design review to Thursday afternoon so the whole team can make it.",
            "Remind me to send the invoice to the client before the end of the week.",
            "One of the two options is better, actually.",
            "Sorry I'm late, the train was delayed.",
            "I mean, it works.",
            "No wait, I'm not ready.",
            "We drew a new line in the sand.",
            "The first and second drafts are done.",
            "Give me one second.",
            "She has two dogs and one cat.",
            "May I ask a question?",
            "I actually think the second one is fine.",
            // Found in review: each of these was rewritten.
            "It's a brand new line.",
            "I'm free on Monday, actually Tuesday too.",
            "I'm busy today, actually tomorrow too.",
            "I was wrong, actually I was right.",
            "It's fine, actually it's great.",
            "This is good, I mean this is great.",
            "We're close, actually we're done.",
            "We need 5, actually 2 more.",
            "We sold 10, actually 5 more than last year.",
            "One coffee, two teas, three waters.",
            "One problem. Two problems. Three problems.",
            "Bullet point is a weird phrase.",
            "First, I'm sorry. Second time's the charm.",
            "I'll march first and you follow.",
        ] {
            assert_eq!(process(said, &options), said);
        }
    }

    #[test]
    fn spoken_rules_are_off_for_other_languages_and_when_switched_off() {
        let mut options = options(&[], &[]);
        let said = "meet at three pm, actually four pm";
        options.english = false;
        assert_eq!(process(said, &options), said);
        options.english = true;
        options.self_corrections = false;
        assert_eq!(process(said, &options), "meet at 3 PM, actually 4 PM");
        options.smart_formatting = false;
        options.voice_commands = false;
        assert_eq!(process("one new line two", &options), "one new line two");
        assert!(is_english("auto") && is_english("en") && !is_english("de"));
    }

    #[test]
    fn process_respects_the_punctuation_toggle() {
        let on = Options {
            smart_trailing_punctuation: true,
            ..options(&[], &[])
        };
        let off = Options {
            smart_trailing_punctuation: false,
            ..options(&[], &[])
        };
        assert_eq!(process("Hello.", &on), "Hello");
        assert_eq!(process("Hello.", &off), "Hello.");
        assert_eq!(process("example.com.", &on), "example.com");
    }
}
