//! Conservative secret redaction applied to transcript text before matching.
//!
//! Redaction runs before a query is compared with any text, so a query can
//! never confirm the presence of a redacted value. The scanners are
//! deliberately biased toward over-redaction.

const PRIVATE_KEY_MARKER: &str = "[REDACTED:private_key]";
const CREDENTIALS_MARKER: &str = "[REDACTED:credentials]";
const TOKEN_MARKER: &str = "[REDACTED:token]";
const SECRET_MARKER: &str = "[REDACTED:secret]";
const REDACTED_PREFIX: &str = "[REDACTED";

/// Known credential prefixes and the minimum token characters that must follow.
const TOKEN_PREFIXES: [(&str, usize); 19] = [
    ("github_pat_", 20),
    ("sk-ant-", 16),
    ("sk-proj-", 16),
    ("sk_live_", 16),
    ("sk_test_", 16),
    ("glpat-", 16),
    ("xoxb-", 16),
    ("xoxp-", 16),
    ("xoxa-", 16),
    ("xoxr-", 16),
    ("xoxs-", 16),
    ("ghp_", 20),
    ("gho_", 20),
    ("ghu_", 20),
    ("ghs_", 20),
    ("ghr_", 20),
    ("npm_", 30),
    ("hf_", 30),
    ("sk-", 20),
];

const CLOUD_KEY_PREFIXES: [&str; 3] = ["AKIA", "ASIA", "AIza"];

const SECRET_KEYWORDS: [&str; 11] = [
    "password",
    "passwd",
    "secret",
    "token",
    "api_key",
    "api-key",
    "apikey",
    "access_key",
    "private_key",
    "authorization",
    "credential",
];

/// Redacted text plus the number of values that were replaced.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Redaction {
    pub text: String,
    pub count: usize,
}

/// Replaces credentials, keys, and secret assignments with fixed markers.
pub fn redact(input: &str) -> Redaction {
    let mut count = 0;
    let text = redact_private_keys(input, &mut count);
    let text = redact_url_credentials(&text, &mut count);
    let text = redact_prefixed_tokens(&text, &mut count);
    let text = redact_authorization_scheme(&text, "bearer ", &mut count);
    let text = redact_authorization_scheme(&text, "basic ", &mut count);
    let text = redact_assignments(&text, &mut count);
    Redaction { text, count }
}

fn redact_private_keys(input: &str, count: &mut usize) -> String {
    const BEGIN: &str = "-----BEGIN ";
    const END: &str = "-----END ";
    const DASHES: &str = "-----";
    let mut output = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find(BEGIN) {
        let header = &rest[start + BEGIN.len()..];
        let is_private_key = header
            .find(DASHES)
            .is_some_and(|end| header[..end].contains("PRIVATE KEY"));
        if !is_private_key {
            output.push_str(&rest[..start + BEGIN.len()]);
            rest = header;
            continue;
        }
        output.push_str(&rest[..start]);
        output.push_str(PRIVATE_KEY_MARKER);
        *count += 1;
        // A key without a terminator is treated as running to the end of the
        // text so a truncated block can never leak its body.
        rest = match header.find(END) {
            Some(end) => {
                let footer = &header[end + END.len()..];
                footer
                    .find(DASHES)
                    .map_or("", |close| &footer[close + DASHES.len()..])
            }
            None => "",
        };
    }
    output.push_str(rest);
    output
}

fn redact_url_credentials(input: &str, count: &mut usize) -> String {
    const SCHEME: &str = "://";
    let mut output = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find(SCHEME) {
        let authority_start = start + SCHEME.len();
        let authority = &rest[authority_start..];
        let authority_end = authority
            .find(|character: char| {
                character.is_whitespace() || matches!(character, '/' | '"' | '\'' | '<' | '>')
            })
            .unwrap_or(authority.len());
        let userinfo = authority[..authority_end]
            .rfind('@')
            .map(|at| &authority[..at])
            .filter(|userinfo| userinfo.contains(':') && !userinfo.starts_with(REDACTED_PREFIX));
        match userinfo {
            Some(userinfo) => {
                output.push_str(&rest[..authority_start]);
                output.push_str(CREDENTIALS_MARKER);
                *count += 1;
                rest = &authority[userinfo.len()..];
            }
            None => {
                output.push_str(&rest[..authority_start]);
                rest = authority;
            }
        }
    }
    output.push_str(rest);
    output
}

fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

fn token_run(bytes: &[u8], accept: impl Fn(u8) -> bool) -> usize {
    bytes.iter().take_while(|byte| accept(**byte)).count()
}

/// Returns the byte length of a credential starting at the beginning of `rest`.
fn credential_length(rest: &str) -> Option<usize> {
    let bytes = rest.as_bytes();
    for (prefix, minimum) in TOKEN_PREFIXES {
        if rest.starts_with(prefix) {
            let run = token_run(&bytes[prefix.len()..], is_token_byte);
            if run >= minimum {
                return Some(prefix.len() + run);
            }
        }
    }
    for prefix in CLOUD_KEY_PREFIXES {
        if rest.starts_with(prefix) {
            let run = token_run(&bytes[prefix.len()..], is_token_byte);
            if run >= 16 {
                return Some(prefix.len() + run);
            }
        }
    }
    if rest.starts_with("eyJ") {
        let run = token_run(bytes, |byte| is_token_byte(byte) || byte == b'.');
        let dots = bytes[..run].iter().filter(|byte| **byte == b'.').count();
        if run >= 30 && dots >= 2 {
            return Some(run);
        }
    }
    None
}

fn redact_prefixed_tokens(input: &str, count: &mut usize) -> String {
    let bytes = input.as_bytes();
    let mut output = String::with_capacity(input.len());
    let mut copied = 0;
    let mut index = 0;
    while index < bytes.len() {
        let at_boundary = index == 0 || !is_token_byte(bytes[index - 1]);
        if at_boundary
            && bytes[index].is_ascii_alphabetic()
            && let Some(length) = credential_length(&input[index..])
        {
            output.push_str(&input[copied..index]);
            output.push_str(TOKEN_MARKER);
            *count += 1;
            index += length;
            copied = index;
            continue;
        }
        index += 1;
    }
    output.push_str(&input[copied..]);
    output
}

/// Redacts the credential that follows an HTTP authorization scheme keyword.
fn redact_authorization_scheme(input: &str, keyword: &str, count: &mut usize) -> String {
    let lowered = input.to_ascii_lowercase();
    let bytes = input.as_bytes();
    let mut output = String::with_capacity(input.len());
    let mut copied = 0;
    let mut search_from = 0;
    while let Some(found) = lowered[search_from..].find(keyword) {
        let value_start = search_from + found + keyword.len();
        let run = token_run(&bytes[value_start..], |byte| {
            is_token_byte(byte) || matches!(byte, b'.' | b'=' | b'+' | b'/' | b'~')
        });
        if run >= 16 {
            output.push_str(&input[copied..value_start]);
            output.push_str(TOKEN_MARKER);
            *count += 1;
            copied = value_start + run;
        }
        search_from = value_start + run;
    }
    output.push_str(&input[copied..]);
    output
}

/// Locates the value of a `keyword = value` or `"keyword": "value"` pair.
///
/// Returns the byte range of the value, excluding any surrounding quotes.
fn assignment_value(input: &str, keyword_end: usize) -> Option<(usize, usize)> {
    let bytes = input.as_bytes();
    let mut index = keyword_end + token_run(&bytes[keyword_end..], is_token_byte);
    if matches!(bytes.get(index), Some(b'"' | b'\'')) {
        index += 1;
    }
    index += token_run(&bytes[index..], |byte| byte == b' ' || byte == b'\t');
    if !matches!(bytes.get(index), Some(b'=' | b':')) {
        return None;
    }
    index += 1;
    index += token_run(&bytes[index..], |byte| byte == b' ' || byte == b'\t');
    match bytes.get(index) {
        Some(quote @ (b'"' | b'\'')) => {
            let start = index + 1;
            let length = token_run(&bytes[start..], |byte| byte != *quote && byte != b'\n');
            (length > 0).then_some((start, start + length))
        }
        Some(_) => {
            let length = token_run(&bytes[index..], |byte| {
                !byte.is_ascii_whitespace() && !matches!(byte, b',' | b';' | b')' | b'}' | b'"')
            });
            let value = &bytes[index..index + length];
            let looks_secret = length >= 12 || value.iter().any(u8::is_ascii_digit);
            (length >= 4 && looks_secret).then_some((index, index + length))
        }
        None => None,
    }
}

fn redact_assignments(input: &str, count: &mut usize) -> String {
    let lowered = input.to_ascii_lowercase();
    let mut output = String::with_capacity(input.len());
    let mut copied = 0;
    let mut index = 0;
    while index < lowered.len() {
        if !lowered.is_char_boundary(index) {
            index += 1;
            continue;
        }
        let keyword = SECRET_KEYWORDS
            .iter()
            .find(|keyword| lowered[index..].starts_with(**keyword));
        let Some(keyword) = keyword else {
            index += 1;
            continue;
        };
        let keyword_end = index + keyword.len();
        match assignment_value(input, keyword_end) {
            Some((start, end))
                if start >= copied && !input[start..].starts_with(REDACTED_PREFIX) =>
            {
                output.push_str(&input[copied..start]);
                output.push_str(SECRET_MARKER);
                *count += 1;
                copied = end;
                index = end;
            }
            _ => index = keyword_end,
        }
    }
    output.push_str(&input[copied..]);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET_CORPUS: [&str; 15] = [
        "sk-ant-api03-ZZcanary0123456789abcdefXY",
        "sk-proj-canary0123456789abcdefgh",
        "ghp_canary0123456789abcdefghijklmnopqr",
        "github_pat_11CANARY0123456789abcdefgh",
        "xoxb-1234567890-canary-abcdefgh",
        concat!("glp", "at-canary0123456789abcd"),
        "AKIACANARY0123456789",
        concat!("AI", "zaCanary0123456789abcdefghijklmnopqrs"),
        concat!(
            "eyJhbGciOiJIUzI1NiJ9",
            ".eyJzdWIiOiJjYW5hcnkifQ",
            ".c2lnbmF0dXJlY2FuYXJ5"
        ),
        "hunter2canary",
        "p4ssw0rd-canary",
        "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjCANARY",
        "dbuser:dbcanarypass",
        "opaque-canary-session-value-0001",
        "dXNlcjpjYW5hcnktYmFzaWMtcGFzcw==",
    ];

    fn corpus_document() -> String {
        [
            "export ANTHROPIC_KEY=sk-ant-api03-ZZcanary0123456789abcdefXY",
            "openai sk-proj-canary0123456789abcdefgh was pasted",
            "git remote uses ghp_canary0123456789abcdefghijklmnopqr",
            "fine-grained github_pat_11CANARY0123456789abcdefgh",
            "slack xoxb-1234567890-canary-abcdefgh",
            concat!("gitlab glp", "at-canary0123456789abcd"),
            "aws AKIACANARY0123456789",
            concat!("maps AI", "zaCanary0123456789abcdefghijklmnopqrs"),
            concat!(
                "jwt eyJhbGciOiJIUzI1NiJ9",
                ".eyJzdWIiOiJjYW5hcnkifQ",
                ".c2lnbmF0dXJlY2FuYXJ5"
            ),
            "password = hunter2canary",
            "{\"db_password\": \"p4ssw0rd-canary\"}",
            concat!(
                "-----BEGIN RSA PRIVATE",
                " KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjCANARY\n-----END RSA PRIVATE",
                " KEY-----"
            ),
            "postgres://dbuser:dbcanarypass@db.internal:5432/app",
            "Authorization: Bearer opaque-canary-session-value-0001",
            concat!(
                "curl -H 'Authorization: Ba",
                "sic dXNlcjpjYW5hcnktYmFzaWMtcGFzcw=='"
            ),
        ]
        .join("\n")
    }

    #[test]
    fn test_redact_removes_every_secret_in_the_corpus() {
        let redaction = redact(&corpus_document());

        for secret in SECRET_CORPUS {
            assert!(
                !redaction.text.contains(secret),
                "secret {secret:?} survived redaction: {}",
                redaction.text
            );
        }
        assert!(redaction.count >= SECRET_CORPUS.len());
        assert!(redaction.text.contains("db.internal:5432/app"));
    }

    #[test]
    fn test_redact_keeps_ordinary_engineering_prose() {
        let prose = "Decided to keep the token budget at 4000 and rename session_store; \
                     see https://example.com/docs/tokens and the skeleton in src/auth.rs. \
                     The password field is validated in the form.";

        let redaction = redact(prose);

        assert_eq!(redaction.text, prose);
        assert_eq!(redaction.count, 0);
    }

    #[test]
    fn test_redact_unterminated_private_key_hides_the_remainder() {
        let redaction =
            redact("key:\n-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA");

        assert_eq!(redaction.text, "key:\n[REDACTED:private_key]");
        assert_eq!(redaction.count, 1);
    }

    #[test]
    fn test_redact_is_idempotent_and_utf8_safe() {
        let input = "naïve café — api_key: \"ключ-secret-值\" then sk-ant-api03-ZZcanary0123456789abcdefXY ✓";

        let once = redact(input);
        let twice = redact(&once.text);

        assert!(!once.text.contains("ключ-secret-值"));
        assert!(once.text.contains("naïve café"));
        assert!(once.text.ends_with('✓'));
        assert_eq!(twice.text, once.text);
        assert_eq!(twice.count, 0);
    }
}
