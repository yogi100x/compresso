//! The clauses of a task, and the part of it that asks about the repository.
//!
//! A task such as "how is X decided, and how is it kept cheap for Y" asks two
//! things. Counting its words over the whole pack lets evidence for one
//! clause stand in for the other, so the planner treats each clause that
//! names a concept as a facet the pack must cover on its own (D95).
//!
//! Two small, deterministic rewrites run before the planner reads any word:
//! a trailing instruction to the answerer ("Cite source files", "Do not
//! modify any files") names nothing in the repository and is dropped, and a
//! prose idiom that no identifier can spell is replaced by the word code uses
//! for it (`IDIOM_BRIDGE`).

/// Prose idioms an identifier cannot spell, and the word code names the same
/// concept with. Kept deliberately tiny: an entry belongs here only when the
/// idiom is common in questions, its words are meaningless on their own
/// (`out`, `date`), and one identifier word is the conventional name for it.
const IDIOM_BRIDGE: &[(&[&str], &str)] = &[
    (&["out", "of", "date"], "stale"),
    (&["up", "to", "date"], "stale"),
    (&["outdated"], "stale"),
];

/// First words of a later clause that instructs the answerer instead of
/// asking about the repository (`don` is `don't` split at the apostrophe).
/// Words that are also common code nouns (`answer`, `reply`, `quote`) are
/// left out.
const DIRECTIVE_LEADS: &[&str] = &["cite", "don", "dont"];

/// Words that open a new question inside one sentence after `and` or `or`:
/// "..., and how is it kept cheap". A comma alone does not split: "size
/// limit, error variant, where raised" is one list.
const QUESTION_LEADS: &[&str] = &[
    "how", "what", "when", "where", "whether", "which", "who", "why",
];

/// The task with later instructions to the answerer dropped and idioms
/// bridged, in clause order. The first clause is always kept: it is the
/// question.
pub(crate) fn task_focus(task: &str) -> String {
    task_clauses(task)
        .into_iter()
        .enumerate()
        .filter(|(index, clause)| *index == 0 || !is_directive(clause))
        .map(|(_, clause)| bridge_idioms(&clause))
        .collect::<Vec<_>>()
        .join(". ")
}

/// Splits a task into clauses at sentence and clause punctuation, at
/// parentheses, and where `and` or `or` opens a new question.
/// Clauses keep their original spelling and are trimmed; empty ones are
/// dropped.
pub(crate) fn task_clauses(task: &str) -> Vec<String> {
    let mut clauses = Vec::new();
    for segment in split_at_punctuation(task) {
        let mut current = Vec::<&str>::new();
        let words = segment.split_whitespace().collect::<Vec<_>>();
        for (index, word) in words.iter().enumerate() {
            let next_opens_question = words
                .get(index + 1)
                .is_some_and(|next| QUESTION_LEADS.contains(&bare_word(next).as_str()));
            let bare = bare_word(word);
            let conjunction = matches!(bare.as_str(), "and" | "or") && next_opens_question;
            if conjunction {
                push_clause(&mut clauses, &current);
                current.clear();
                continue;
            }
            current.push(word);
        }
        push_clause(&mut clauses, &current);
    }
    clauses
}

fn push_clause(clauses: &mut Vec<String>, words: &[&str]) {
    let clause = words
        .join(" ")
        .trim_matches(|character: char| character == ',' || character.is_whitespace())
        .to_owned();
    if !clause.is_empty() {
        clauses.push(clause);
    }
}

/// Text between `.`, `;`, `:`, `?`, or `!` followed by whitespace or the
/// end, and at parentheses and line breaks. `file.rs` and `a::b` stay whole.
fn split_at_punctuation(task: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut start = 0;
    let mut characters = task.char_indices().peekable();
    while let Some((index, character)) = characters.next() {
        let next = characters.peek().map(|(_, next)| *next);
        let ends = match character {
            '(' | ')' | '[' | ']' | '\n' => true,
            '.' | ';' | ':' | '?' | '!' => next.is_none_or(char::is_whitespace),
            _ => false,
        };
        if ends {
            segments.push(&task[start..index]);
            start = index + character.len_utf8();
        }
    }
    segments.push(&task[start..]);
    segments
        .into_iter()
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .collect()
}

fn bare_word(word: &str) -> String {
    word.trim_matches(|character: char| !character.is_alphanumeric())
        .to_ascii_lowercase()
}

fn is_directive(clause: &str) -> bool {
    let words = clause
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    let words = match words.first().map(String::as_str) {
        Some("please") => &words[1..],
        _ => &words[..],
    };
    match words {
        [first, second, ..] if first == "do" && second == "not" => true,
        [first, ..] => DIRECTIVE_LEADS.contains(&first.as_str()),
        [] => false,
    }
}

/// Replaces each `IDIOM_BRIDGE` idiom spelled as whole words, in any case
/// and with spaces or hyphens between its words, by its code word.
fn bridge_idioms(clause: &str) -> String {
    // Words are runs of letters, digits, and `_`, so `out_of_date` is one
    // identifier and never an idiom.
    let mut words = Vec::<(usize, usize)>::new();
    let mut start = None;
    for (index, character) in clause.char_indices() {
        let word_character = character.is_alphanumeric() || character == '_';
        match (word_character, start) {
            (true, None) => start = Some(index),
            (false, Some(begin)) => {
                words.push((begin, index));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(begin) = start {
        words.push((begin, clause.len()));
    }

    let mut output = String::with_capacity(clause.len());
    let mut copied = 0;
    let mut index = 0;
    while index < words.len() {
        let matched = IDIOM_BRIDGE.iter().find(|(idiom, _)| {
            words.get(index..index + idiom.len()).is_some_and(|window| {
                window
                    .iter()
                    .zip(idiom.iter())
                    .all(|(&(begin, end), expected)| {
                        clause[begin..end].eq_ignore_ascii_case(expected)
                    })
                    && window.windows(2).all(|pair| {
                        clause[pair[0].1..pair[1].0]
                            .chars()
                            .all(|gap| gap.is_whitespace() || gap == '-')
                    })
            })
        });
        if let Some((idiom, replacement)) = matched {
            output.push_str(&clause[copied..words[index].0]);
            output.push_str(replacement);
            copied = words[index + idiom.len() - 1].1;
            index += idiom.len();
        } else {
            index += 1;
        }
    }
    output.push_str(&clause[copied..]);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clauses_split_at_punctuation_and_at_a_new_question() {
        assert_eq!(
            task_clauses(
                "Where do we reject a payload that is too large, and what error does the caller get?"
            ),
            vec![
                "Where do we reject a payload that is too large",
                "what error does the caller get"
            ]
        );
        assert_eq!(
            task_clauses("staleness check: how is it stale (mtime, size), cheap for large files"),
            vec![
                "staleness check",
                "how is it stale",
                "mtime, size",
                "cheap for large files"
            ]
        );
    }

    #[test]
    fn test_clauses_keep_file_names_paths_and_plain_conjunctions_whole() {
        assert_eq!(
            task_clauses("How does main.rs read the payload file and submit the job?"),
            vec!["How does main.rs read the payload file and submit the job"]
        );
        assert_eq!(
            task_clauses("Who calls crate::queries::plan_context"),
            vec!["Who calls crate::queries::plan_context"]
        );
    }

    #[test]
    fn test_focus_drops_instructions_to_the_answerer() {
        assert_eq!(
            task_focus(
                "Explain how the lease expires. Cite source files and function names. Do not modify any files."
            ),
            "Explain how the lease expires"
        );
        assert_eq!(
            task_focus("Where is retry? Please don't edit anything."),
            "Where is retry"
        );
        assert_eq!(
            task_focus("Cite every caller of retry. Do not guess."),
            "Cite every caller of retry",
            "the first clause is the question, whatever its first word"
        );
        assert_eq!(
            task_focus("reply parsing: where are answer frames decoded?"),
            "reply parsing. where are answer frames decoded",
            "code nouns are not instructions"
        );
    }

    #[test]
    fn test_focus_bridges_idioms_no_identifier_can_spell() {
        assert_eq!(
            task_focus("Is the stored outline out of date compared with disk?"),
            "Is the stored outline stale compared with disk"
        );
        assert_eq!(
            task_focus("when is an Out-Of-Date, or outdated, cache rebuilt"),
            "when is an stale, or stale, cache rebuilt"
        );
        assert_eq!(
            task_focus("How do we know the outline is up to date?"),
            "How do we know the outline is stale",
            "checking that something is up to date is a staleness check"
        );
        assert_eq!(
            task_focus("keep the date out of the log"),
            "keep the date out of the log",
            "the idiom's words apart are not the idiom"
        );
        assert_eq!(
            task_focus("read out_of_date flags"),
            "read out_of_date flags",
            "an identifier is never rewritten"
        );
    }
}
