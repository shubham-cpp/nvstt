//! Conservative spoken-number formatting. Never infer a time from a range.
use text_processing_rs::{NormalizeOptions, normalize_with_options};

fn number_word(word: &str) -> bool {
    matches!(
        word,
        "zero"
            | "oh"
            | "one"
            | "two"
            | "three"
            | "four"
            | "five"
            | "six"
            | "seven"
            | "eight"
            | "nine"
            | "ten"
            | "eleven"
            | "twelve"
            | "thirteen"
            | "fourteen"
            | "fifteen"
            | "sixteen"
            | "seventeen"
            | "eighteen"
            | "nineteen"
            | "twenty"
            | "thirty"
            | "forty"
            | "fifty"
            | "sixty"
            | "seventy"
            | "eighty"
            | "ninety"
            | "hundred"
            | "thousand"
            | "million"
            | "billion"
            | "lakh"
            | "crore"
    )
}

fn scale(word: &str) -> bool {
    matches!(
        word,
        "hundred" | "thousand" | "million" | "billion" | "lakh" | "crore"
    )
}

fn core(token: &str) -> &str {
    token.trim_matches(|c: char| matches!(c, ',' | '.' | '!' | '?' | ';' | ':' | '"' | '(' | ')'))
}

/// Format number spans only. Punctuation, technical tokens, ranges, and
/// ordinary conjunctions remain boundaries. The library handles arithmetic.
pub(crate) fn normalize_numbers(text: &str) -> String {
    let tokens: Vec<_> = text.split_whitespace().collect();
    let mut output = Vec::with_capacity(tokens.len());
    let mut index = 0;
    while index < tokens.len() {
        let start = index;
        let first = core(tokens[start]);
        let lower = first.to_ascii_lowercase();
        let next_is_number = |i: usize| tokens.get(i).is_some_and(|t| number_word(core(t)));
        // Uppercase words may be acronyms or literal identifiers.
        let uppercase = first.len() > 1 && first.bytes().all(|b| b.is_ascii_uppercase());
        let signed = matches!(lower.as_str(), "minus" | "negative") && next_is_number(start + 1);
        if uppercase || (!number_word(&lower) && !signed) || lower == "oh" {
            output.push(tokens[index].to_owned());
            index += 1;
            continue;
        }
        index += 1;
        while index < tokens.len() && index - start < 32 {
            let previous = core(tokens[index - 1]);
            if !tokens[index - 1].ends_with(previous) {
                break;
            }
            let current = core(tokens[index]);
            if !tokens[index].starts_with(current) {
                break;
            }
            let connector = current == "point" || (current == "and" && scale(previous));
            if number_word(current) || (connector && next_is_number(index + 1)) {
                index += 1;
            } else {
                break;
            }
        }
        let last = core(tokens[index - 1]);
        let phrase = tokens[start..index]
            .iter()
            .map(|t| core(t).to_ascii_lowercase())
            .collect::<Vec<_>>()
            .join(" ");
        let normalized = normalize_with_options(
            &phrase,
            NormalizeOptions::new()
                .with_disable_bare_second(true)
                .with_concat_compound_numbers(true),
        );
        // Conversion must produce a number. Otherwise keep the spoken form.
        if !normalized.chars().any(|c| c.is_ascii_digit())
            || !normalized
                .chars()
                .all(|c| c.is_ascii_digit() || matches!(c, '.' | ',' | '-' | '+'))
        {
            output.extend(tokens[start..index].iter().map(|s| (*s).to_owned()));
            continue;
        }
        let leading = &tokens[start][..tokens[start].find(first).unwrap_or(0)];
        let trailing = &tokens[index - 1][tokens[index - 1].find(last).unwrap_or(0) + last.len()..];
        output.push(format!("{leading}{normalized}{trailing}"));
    }
    output.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_numbers_without_reinterpreting_other_words() {
        for (spoken, written) in [
            ("I have twenty one apples", "I have 21 apples"),
            ("five to eight runs", "5 to 8 runs"),
            ("one and two", "1 and 2"),
            ("one hundred and twenty five", "125"),
            ("minus five point two", "-5.2"),
            ("(Twenty one), then two.", "(21), then 2."),
            ("give me a second", "give me a second"),
            ("DOT and .env use C++", "DOT and .env use C++"),
            ("ONE is an identifier", "ONE is an identifier"),
            ("oh that worked", "oh that worked"),
            ("one, two", "1, 2"),
            ("twenty dollars", "20 dollars"),
            ("one two three four", "1234"),
            ("three four", "34"),
            ("twenty twenty five", "2025"),
        ] {
            assert_eq!(normalize_numbers(spoken), written, "{spoken}");
        }
    }
}
