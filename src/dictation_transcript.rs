//! Dictation content after a finished recognition.
//!
//! This is linguistic cleanup of a transcript. It is not the speech gate.
//! The speech gate routes audio. This module decides whether finished
//! recognition has text worth keeping. Ambiguous tokens remain.
//!
//! Order: collapse short stutters, strip filled pauses, apply user
//! replacements, then optionally normalize spoken forms.

use serde::{Deserialize, Deserializer, Serialize, Serializer, ser::SerializeMap};
use text_processing_rs::{NormalizeOptions, normalize_sentence_with_options};

use crate::recognizer::RecognitionOutcome;

/// Spoken hesitation words the English models often emit as tokens.
/// Match whole tokens only. Do not treat these as prefixes.
const FILLED_PAUSES: &[&str] = &["uh", "uhh", "uhhh", "um", "umm", "ummm"];

/// Consecutive copies of a 1 or 2 letter token that the recognizer looped.
const STUTTER_RUN: usize = 3;
const STUTTER_MAX_LETTERS: usize = 2;

/// Finished dictation content after cleanup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DictationTranscript {
    /// Non-empty transcript ready for history and final delivery.
    Ready(String),
    /// No delivery and no history record.
    /// Covers speech-gate `NoSpeech` and filler-only speech.
    NoContent,
}

/// ASR returned a transcript that is empty after trim, before filler policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EmptyTranscript;

#[derive(Clone, Debug, PartialEq, Eq)]
struct ReplacementRule {
    pattern: Vec<String>,
    replacement: String,
}

/// User-defined substitutions applied after stutter collapse and filled pauses.
///
/// Patterns match whole tokens, case-insensitively. Longer patterns win.
/// The replacement text is used as written.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Replacements {
    rules: Vec<ReplacementRule>,
}

impl Replacements {
    pub fn from_pairs(pairs: impl IntoIterator<Item = (String, String)>) -> Self {
        let mut rules: Vec<ReplacementRule> = pairs
            .into_iter()
            .filter_map(|(pattern, replacement)| {
                let tokens: Vec<String> = pattern
                    .split_whitespace()
                    .map(|token| word_core(token).to_ascii_lowercase())
                    .filter(|token| !token.is_empty())
                    .collect();
                if tokens.is_empty() {
                    return None;
                }
                Some(ReplacementRule {
                    pattern: tokens,
                    replacement,
                })
            })
            .collect();
        rules.sort_by(|left, right| {
            right.pattern.len().cmp(&left.pattern.len()).then_with(|| {
                let right_chars: usize = right.pattern.iter().map(String::len).sum();
                let left_chars: usize = left.pattern.iter().map(String::len).sum();
                right_chars.cmp(&left_chars)
            })
        });
        Self { rules }
    }

    fn match_at(&self, tokens: &[&str], index: usize) -> Option<(usize, &str)> {
        self.rules.iter().find_map(|rule| {
            let end = index.checked_add(rule.pattern.len())?;
            if end > tokens.len() {
                return None;
            }
            let matched = rule.pattern.iter().enumerate().all(|(offset, expected)| {
                word_core(tokens[index + offset]).eq_ignore_ascii_case(expected)
            });
            matched.then_some((rule.pattern.len(), rule.replacement.as_str()))
        })
    }
}

impl Serialize for Replacements {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.rules.len()))?;
        for rule in &self.rules {
            map.serialize_entry(&rule.pattern.join(" "), &rule.replacement)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Replacements {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let pairs = std::collections::BTreeMap::<String, String>::deserialize(deserializer)?;
        Ok(Self::from_pairs(pairs))
    }
}

/// Map a finished recognition onto deliverable dictation content.
///
/// Pure. No I/O. Does not log text.
pub fn dictation_transcript(
    outcome: RecognitionOutcome,
    replacements: &Replacements,
    itn: bool,
) -> Result<DictationTranscript, EmptyTranscript> {
    let RecognitionOutcome::Transcript(raw) = outcome else {
        return Ok(DictationTranscript::NoContent);
    };

    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(EmptyTranscript);
    }

    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    let collapsed = collapse_stutters(&tokens);
    let without_fillers: Vec<&str> = collapsed
        .into_iter()
        .filter(|token| !is_filled_pause(token))
        .collect();
    let replaced = apply_replacements(&without_fillers, replacements).join(" ");
    let cleaned = if itn {
        normalize_sentence_with_options(
            &replaced,
            NormalizeOptions::new().with_disable_bare_second(true),
        )
    } else {
        replaced
    };
    if cleaned.is_empty() {
        Ok(DictationTranscript::NoContent)
    } else {
        Ok(DictationTranscript::Ready(cleaned))
    }
}

fn collapse_stutters<'a>(tokens: &[&'a str]) -> Vec<&'a str> {
    let mut output = Vec::with_capacity(tokens.len());
    let mut index = 0;
    while index < tokens.len() {
        let core = word_core(tokens[index]);
        if is_short_stutter_token(core) {
            let mut run = 1;
            while index + run < tokens.len()
                && is_short_stutter_token(word_core(tokens[index + run]))
                && word_core(tokens[index + run]).eq_ignore_ascii_case(core)
            {
                run += 1;
            }
            if run >= STUTTER_RUN {
                output.push(tokens[index]);
                index += run;
                continue;
            }
        }
        output.push(tokens[index]);
        index += 1;
    }
    output
}

fn is_short_stutter_token(core: &str) -> bool {
    (1..=STUTTER_MAX_LETTERS).contains(&core.len())
        && core.bytes().all(|byte| byte.is_ascii_alphabetic())
        && !(core.len() >= 2 && core.bytes().all(|byte| byte.is_ascii_uppercase()))
}

fn apply_replacements(tokens: &[&str], replacements: &Replacements) -> Vec<String> {
    let mut output = Vec::with_capacity(tokens.len());
    let mut index = 0;
    while index < tokens.len() {
        if let Some((length, replacement)) = replacements.match_at(tokens, index) {
            if !replacement.is_empty() {
                output.push(replacement.to_owned());
            }
            index += length;
        } else {
            output.push(tokens[index].to_owned());
            index += 1;
        }
    }
    output
}

fn is_filled_pause(token: &str) -> bool {
    let core = word_core(token);
    let lowercase = core.bytes().all(|b| b.is_ascii_lowercase());
    let title_case = core.as_bytes().first().is_some_and(u8::is_ascii_uppercase)
        && core.as_bytes()[1..].iter().all(u8::is_ascii_lowercase);
    (lowercase || title_case)
        && FILLED_PAUSES.iter().any(|filler| core.eq_ignore_ascii_case(filler))
}

#[derive(Clone, Copy, Debug)]
struct TokenParts<'a> {
    core: &'a str,
}

fn token_parts(token: &str) -> TokenParts<'_> {
    let rest = token.trim_start_matches(|c: char| {
        matches!(c, '"' | '\'' | '“' | '‘' | '(' | '[' | '{')
    });
    let core = rest.trim_end_matches(|c: char| {
        matches!(
            c,
            '"' | '\'' | '”' | '’' | ')' | ']' | '}' | ',' | '.' | '!' | '?' | ';' | ':'
        )
    });
    if core.is_empty() {
        return TokenParts { core: token };
    }
    TokenParts { core }
}

fn word_core(token: &str) -> &str {
    token_parts(token).core
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transcript(text: &str) -> RecognitionOutcome {
        RecognitionOutcome::Transcript(text.to_owned())
    }

    fn clean(text: &str) -> Result<DictationTranscript, EmptyTranscript> {
        dictation_transcript(transcript(text), &Replacements::default(), false)
    }

    fn clean_with(
        text: &str,
        pairs: &[(&str, &str)],
    ) -> Result<DictationTranscript, EmptyTranscript> {
        let replacements = Replacements::from_pairs(
            pairs
                .iter()
                .map(|(pattern, replacement)| ((*pattern).to_owned(), (*replacement).to_owned())),
        );
        dictation_transcript(transcript(text), &replacements, false)
    }

    #[test]
    fn technical_tokens_survive_cleanup_with_and_without_itn() {
        for itn in [false, true] {
            for text in [
                "5 mm", "ER diagram", "a + b = c", "C++ C# .env config.rs",
                "ER ER ER", "C C++ C", "very very", "UH UM",
            ] {
                assert_eq!(
                    dictation_transcript(transcript(text), &Replacements::default(), itn),
                    Ok(DictationTranscript::Ready(text.to_owned())),
                    "input={text:?}, itn={itn}",
                );
            }
        }
    }

    #[test]
    fn punctuation_only_input_is_not_silently_discarded() {
        assert_eq!(
            clean("+ = /"),
            Ok(DictationTranscript::Ready("+ = /".to_owned()))
        );
    }

    #[test]
    fn uppercase_token_interrupts_a_stutter_run() {
        assert_eq!(
            clean("no NO no"),
            Ok(DictationTranscript::Ready("no NO no".to_owned()))
        );
        assert_eq!(
            clean("er ER er"),
            Ok(DictationTranscript::Ready("er ER er".to_owned()))
        );
    }

    #[test]
    fn no_speech_has_no_content() {
        assert_eq!(
            dictation_transcript(
                RecognitionOutcome::NoSpeech,
                &Replacements::default(),
                false,
            ),
            Ok(DictationTranscript::NoContent)
        );
    }

    #[test]
    fn empty_or_whitespace_is_a_transcription_failure() {
        assert_eq!(clean(""), Err(EmptyTranscript));
        assert_eq!(clean("   \n\t"), Err(EmptyTranscript));
    }

    #[test]
    fn strips_filled_pauses_and_keeps_content() {
        assert_eq!(
            clean("uh hello um world"),
            Ok(DictationTranscript::Ready("hello world".to_owned()))
        );
    }

    #[test]
    fn filler_only_has_no_content() {
        assert_eq!(clean("um uh um."), Ok(DictationTranscript::NoContent));
        assert_eq!(clean("Uh, um..."), Ok(DictationTranscript::NoContent));
    }

    #[test]
    fn drops_edge_punctuation_on_filler_tokens() {
        assert_eq!(
            clean("um, hello"),
            Ok(DictationTranscript::Ready("hello".to_owned()))
        );
        assert_eq!(
            clean("hello, uh"),
            Ok(DictationTranscript::Ready("hello,".to_owned()))
        );
    }

    #[test]
    fn keeps_content_that_looks_like_a_filler_prefix() {
        assert_eq!(
            clean("uh-huh"),
            Ok(DictationTranscript::Ready("uh-huh".to_owned()))
        );
        assert_eq!(
            clean("umbrella"),
            Ok(DictationTranscript::Ready("umbrella".to_owned()))
        );
    }

    #[test]
    fn keeps_short_content_words() {
        assert_eq!(
            clean("I need a minute"),
            Ok(DictationTranscript::Ready("I need a minute".to_owned()))
        );
        assert_eq!(
            clean("oh no"),
            Ok(DictationTranscript::Ready("oh no".to_owned()))
        );
    }

    #[test]
    fn uppercase_acronyms_are_not_fillers() {
        assert_eq!(
            clean("UH Hello UM"),
            Ok(DictationTranscript::Ready("UH Hello UM".to_owned()))
        );
        assert_eq!(
            clean("uh Hello Um"),
            Ok(DictationTranscript::Ready("Hello".to_owned()))
        );
    }

    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(
            clean("  hello world  "),
            Ok(DictationTranscript::Ready("hello world".to_owned()))
        );
    }

    #[test]
    fn does_not_strip_like_or_you_know() {
        assert_eq!(
            clean("like you know hello"),
            Ok(DictationTranscript::Ready("like you know hello".to_owned()))
        );
    }

    #[test]
    fn collapses_short_recognizer_stutters() {
        assert_eq!(
            clean("wh wh wh wh why"),
            Ok(DictationTranscript::Ready("wh why".to_owned()))
        );
        assert_eq!(
            clean("I I I I think"),
            Ok(DictationTranscript::Ready("I think".to_owned()))
        );
    }

    #[test]
    fn keeps_a_double_short_word() {
        assert_eq!(
            clean("no no thanks"),
            Ok(DictationTranscript::Ready("no no thanks".to_owned()))
        );
    }

    #[test]
    fn does_not_collapse_longer_repeated_words() {
        assert_eq!(
            clean("hello hello hello hello"),
            Ok(DictationTranscript::Ready(
                "hello hello hello hello".to_owned()
            ))
        );
    }

    #[test]
    fn replacements_are_case_insensitive_and_keep_specified_text() {
        assert_eq!(
            clean_with("the nv stt daemon", &[("nv stt", "nvstt")]),
            Ok(DictationTranscript::Ready("the nvstt daemon".to_owned()))
        );
        assert_eq!(
            clean_with("ParaKeet is fast", &[("parakeet", "Parakeet")]),
            Ok(DictationTranscript::Ready("Parakeet is fast".to_owned()))
        );
    }

    #[test]
    fn longer_replacement_wins() {
        assert_eq!(
            clean_with("dot com site", &[("dot", "."), ("dot com", ".com")],),
            Ok(DictationTranscript::Ready(".com site".to_owned()))
        );
    }

    #[test]
    fn replacements_run_after_fillers_and_stutters() {
        assert_eq!(
            clean_with("uh nv uh stt", &[("nv stt", "nvstt")]),
            Ok(DictationTranscript::Ready("nvstt".to_owned()))
        );
        assert_eq!(
            clean_with("I I I I nv stt", &[("nv stt", "nvstt")]),
            Ok(DictationTranscript::Ready("I nvstt".to_owned()))
        );
    }

    #[test]
    fn empty_replacement_deletes_the_pattern() {
        assert_eq!(
            clean_with("please scratch that hello", &[("scratch that", "")]),
            Ok(DictationTranscript::Ready("please hello".to_owned()))
        );
    }

    #[test]
    fn inverse_text_normalization_runs_after_replacements() {
        let replacements =
            Replacements::from_pairs([("a dozen".to_owned(), "twenty one".to_owned())]);
        assert_eq!(
            dictation_transcript(transcript("I have a dozen apples"), &replacements, true),
            Ok(DictationTranscript::Ready("I have 21 apples".to_owned()))
        );
    }

    #[test]
    fn inverse_text_normalization_keeps_bare_second() {
        assert_eq!(
            dictation_transcript(
                transcript("give me a second"),
                &Replacements::default(),
                true,
            ),
            Ok(DictationTranscript::Ready("give me a second".to_owned()))
        );
    }

    #[test]
    fn inverse_text_normalization_can_be_disabled() {
        assert_eq!(
            dictation_transcript(
                transcript("I have twenty one apples"),
                &Replacements::default(),
                false,
            ),
            Ok(DictationTranscript::Ready(
                "I have twenty one apples".to_owned()
            ))
        );
    }
}
