//! Scrubbing every part of the configured URL out of engine messages.
//!
//! Driver and server errors echo the user name, host, database name, and
//! sometimes the whole connection string. Messages that reach a tool result
//! pass through [`Secrets::scrub`], which removes each URL component in raw,
//! percent-encoded, and percent-decoded form, any `scheme://` URL, and any
//! `password=` value, then bounds the length.

use std::fmt;

use percent_encoding::{NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use url::Url;

const REDACTED: &str = "[redacted]";
/// Longest message kept after scrubbing.
const MAX_MESSAGE_CHARS: usize = 600;
/// Components shorter than this are left alone: scrubbing `db` or a one-letter
/// user would mangle ordinary words without protecting anything.
const MIN_SECRET_CHARS: usize = 3;

/// The strings one target's messages must never contain.
#[derive(Clone, Default)]
pub(crate) struct Secrets(Vec<String>);

impl fmt::Debug for Secrets {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Secrets({} redacted)", self.0.len())
    }
}

impl Secrets {
    /// Collects the whole value, user, password, host, database path, and
    /// the values of the named query parameters, in every encoding.
    pub(crate) fn from_url(value: &str, url: &Url, secret_params: &[&str]) -> Self {
        let mut secrets = Self::default();
        secrets.add(value);
        secrets.add(url.as_str());
        secrets.add(url.username());
        if let Some(password) = url.password() {
            secrets.add(password);
        }
        if let Some(host) = url.host_str() {
            secrets.add(host);
        }
        let path = url.path().trim_start_matches('/');
        secrets.add(path);
        for (key, value) in url.query_pairs() {
            if secret_params.contains(&key.as_ref()) {
                secrets.add(&value);
            }
        }
        secrets
    }

    /// Adds one component in raw, decoded, and encoded form.
    pub(crate) fn add(&mut self, component: &str) {
        let decoded = percent_decode_str(component)
            .decode_utf8_lossy()
            .into_owned();
        let encoded = utf8_percent_encode(&decoded, NON_ALPHANUMERIC).to_string();
        for form in [component.to_owned(), decoded, encoded] {
            if form.chars().count() >= MIN_SECRET_CHARS && !self.0.contains(&form) {
                self.0.push(form);
            }
        }
        // Longest first, so a component inside another is not left half
        // replaced.
        self.0.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
    }

    /// The message with every secret, URL, and password value removed.
    pub(crate) fn scrub(&self, message: &str) -> String {
        let mut text = message.to_owned();
        for secret in &self.0 {
            text = replace_ignore_ascii_case(&text, secret);
        }
        bound(&scrub_password_values(&scrub_urls(&text)))
    }
}

/// Replaces `needle` in `haystack`, ignoring ASCII case.
fn replace_ignore_ascii_case(haystack: &str, needle: &str) -> String {
    if needle.is_empty() {
        return haystack.to_owned();
    }
    let lower_haystack = haystack.to_ascii_lowercase();
    let lower_needle = needle.to_ascii_lowercase();
    let mut output = String::with_capacity(haystack.len());
    let mut cursor = 0;
    while let Some(found) = lower_haystack[cursor..].find(&lower_needle) {
        let start = cursor + found;
        output.push_str(&haystack[cursor..start]);
        output.push_str(REDACTED);
        cursor = start + needle.len();
    }
    output.push_str(&haystack[cursor..]);
    output
}

/// Replaces every `scheme://...` run up to whitespace or a quote.
fn scrub_urls(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(found) = rest.find("://") {
        let scheme_start = rest[..found]
            .rfind(|character: char| {
                !(character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.'))
            })
            .map_or(0, |index| index + 1);
        let after = &rest[found + 3..];
        let end = after
            .find(|character: char| {
                character.is_whitespace() || matches!(character, '"' | '\'' | '`')
            })
            .unwrap_or(after.len());
        output.push_str(&rest[..scheme_start]);
        output.push_str(REDACTED);
        rest = &after[end..];
    }
    output.push_str(rest);
    output
}

/// Replaces the value after `password=` or `password:` (any case).
fn scrub_password_values(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut output = String::with_capacity(text.len());
    let mut cursor = 0;
    while let Some(found) = lower[cursor..].find("password") {
        let key_end = cursor + found + "password".len();
        let mut value_start = key_end;
        let bytes = text.as_bytes();
        while value_start < text.len() && matches!(bytes[value_start], b' ') {
            value_start += 1;
        }
        if value_start < text.len() && matches!(bytes[value_start], b'=' | b':') {
            value_start += 1;
            while value_start < text.len() && bytes[value_start] == b' ' {
                value_start += 1;
            }
            let value_end = text[value_start..]
                .find(|character: char| {
                    character.is_whitespace() || matches!(character, '&' | ';' | ',')
                })
                .map_or(text.len(), |index| value_start + index);
            output.push_str(&text[cursor..value_start]);
            output.push_str(REDACTED);
            cursor = value_end;
        } else {
            output.push_str(&text[cursor..key_end]);
            cursor = key_end;
        }
    }
    output.push_str(&text[cursor..]);
    output
}

fn bound(text: &str) -> String {
    let single_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.chars().count() <= MAX_MESSAGE_CHARS {
        return single_line;
    }
    let mut cut = single_line
        .chars()
        .take(MAX_MESSAGE_CHARS)
        .collect::<String>();
    cut.push_str(" [...]");
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWORD: &str = concat!("c4n4ry", "-", "Pa55", "!word");
    const USER: &str = concat!("canary", "_", "user");
    const HOST: &str = concat!("canary", "-host", ".example.com");
    const DATABASE: &str = concat!("canary", "_db");

    fn secrets() -> (Secrets, String) {
        let encoded_password = utf8_percent_encode(PASSWORD, NON_ALPHANUMERIC).to_string();
        let value = format!("postgres://{USER}:{encoded_password}@{HOST}:5432/{DATABASE}");
        let url = Url::parse(&value).expect("url");
        (Secrets::from_url(&value, &url, &["password"]), value)
    }

    fn assert_clean(text: &str) {
        let encoded_password = utf8_percent_encode(PASSWORD, NON_ALPHANUMERIC).to_string();
        for canary in [PASSWORD, encoded_password.as_str(), USER, HOST, DATABASE] {
            assert!(!text.contains(canary), "{canary} in {text}");
            assert!(
                !text
                    .to_ascii_lowercase()
                    .contains(&canary.to_ascii_lowercase()),
                "{canary} in {text}"
            );
        }
    }

    #[test]
    fn test_every_url_component_is_scrubbed_in_every_form() {
        let (secrets, value) = secrets();
        let encoded_password = utf8_percent_encode(PASSWORD, NON_ALPHANUMERIC).to_string();
        for message in [
            format!("password authentication failed for user \"{USER}\""),
            format!("could not connect to {HOST}:5432: refused"),
            format!("database \"{DATABASE}\" does not exist"),
            format!("error with configuration: {value}"),
            format!("bad password {PASSWORD} and {encoded_password}"),
            format!(
                "{} at {}",
                USER.to_ascii_uppercase(),
                HOST.to_ascii_uppercase()
            ),
            "connect postgresql://someone:other@elsewhere/x failed".to_owned(),
            "dsn password=hunter2xyz host=a".to_owned(),
        ] {
            let scrubbed = secrets.scrub(&message);
            assert_clean(&scrubbed);
            assert!(!scrubbed.contains("hunter2xyz"), "{scrubbed}");
            assert!(!scrubbed.contains("someone:other"), "{scrubbed}");
        }
    }

    #[test]
    fn test_scrub_keeps_ordinary_text_and_bounds_length() {
        let (secrets, _) = secrets();
        assert_eq!(
            secrets.scrub("relation \"orders\" does not exist"),
            "relation \"orders\" does not exist"
        );
        let long = "x ".repeat(2_000);
        assert!(secrets.scrub(&long).chars().count() <= MAX_MESSAGE_CHARS + 6);
        assert!(!format!("{secrets:?}").contains(PASSWORD));
    }
}
