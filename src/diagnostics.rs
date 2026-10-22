//! Shared diagnostic redaction for messages that can cross process or service boundaries.

fn sensitive_query_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "token"
            | "access_token"
            | "access-token"
            | "refresh_token"
            | "refresh-token"
            | "id_token"
            | "id-token"
            | "auth"
            | "authorization"
            | "api_key"
            | "api-key"
            | "apikey"
            | "clientsecret"
            | "client_secret"
            | "client-secret"
            | "password"
            | "secret"
            | "signature"
            | "sig"
            | "hm"
            | "x-amz-credential"
            | "x-amz-signature"
            | "x-amz-security-token"
            | "x-goog-credential"
            | "x-goog-signature"
    )
}

const SENSITIVE_CREDENTIAL_LABELS: &[&str] = &[
    "authorization",
    "access_token",
    "access-token",
    "refresh_token",
    "refresh-token",
    "id_token",
    "id-token",
    "clientsecret",
    "client_secret",
    "client-secret",
    "api_key",
    "api-key",
    "apikey",
    "password",
    "credential",
    "secret",
    "token",
    "signature",
    "sig",
    "hm",
];

fn redact_url_secrets(raw: &str) -> String {
    let Ok(mut url) = url::Url::parse(raw) else {
        return raw.to_owned();
    };
    let mut changed = false;
    if !url.username().is_empty() {
        let _ = url.set_username("[secret redacted]");
        changed = true;
    }
    if !url.password().unwrap_or_default().is_empty() {
        let _ = url.set_password(Some("[secret redacted]"));
        changed = true;
    }
    let query_pairs: Vec<_> = url
        .query_pairs()
        .map(|(key, value)| {
            if sensitive_query_key(&key) && !value.is_empty() {
                changed = true;
                (key.into_owned(), "[secret redacted]".to_owned())
            } else {
                (key.into_owned(), value.into_owned())
            }
        })
        .collect();
    if changed && url.query().is_some() {
        let mut query = url.query_pairs_mut();
        query.clear();
        for (key, value) in query_pairs {
            query.append_pair(&key, &value);
        }
    }
    if changed {
        url.to_string()
    } else {
        raw.to_owned()
    }
}

fn redact_embedded_urls(detail: &str) -> String {
    let mut output = String::with_capacity(detail.len());
    let mut rest = detail;
    while let Some(start) = rest.find("https://").or_else(|| rest.find("http://")) {
        output.push_str(&rest[..start]);
        let candidate = &rest[start..];
        let end = candidate
            .find(|character: char| {
                character.is_whitespace() || ['"', '\'', '<', '>'].contains(&character)
            })
            .unwrap_or(candidate.len());
        let (url, tail) = candidate.split_at(end);
        let trimmed_url = url.trim_end_matches([',', ';', '.', ')', ']', '}']);
        let punctuation = &url[trimmed_url.len()..];
        output.push_str(&redact_url_secrets(trimmed_url));
        output.push_str(punctuation);
        rest = tail;
    }
    output.push_str(rest);
    output
}

fn redact_authorization_values(detail: &str) -> String {
    let lower = detail.to_ascii_lowercase();
    let mut output = String::with_capacity(detail.len());
    let mut rest = detail;
    let mut lower_rest = lower.as_str();
    loop {
        let header = lower_rest.find("authorization:");
        let bearer = lower_rest.find("bearer ");
        let (index, is_header) = match (header, bearer) {
            (Some(header), Some(bearer)) if header <= bearer => (header, true),
            (Some(_), Some(bearer)) => (bearer, false),
            (Some(header), None) => (header, true),
            (None, Some(bearer)) => (bearer, false),
            (None, None) => break,
        };
        output.push_str(&rest[..index]);
        if is_header {
            let value_start = index + "authorization:".len();
            let value_end = lower_rest[value_start..]
                .find(['\r', '\n', ',', '}', '"'])
                .map(|offset| value_start + offset)
                .unwrap_or(rest.len());
            output.push_str(&rest[index..value_start]);
            let value = rest[value_start..value_end].trim_start();
            let scheme_end = value.find(char::is_whitespace).unwrap_or(value.len());
            let scheme = &value[..scheme_end];
            if matches!(scheme.to_ascii_lowercase().as_str(), "bearer" | "basic")
                && scheme_end < value.len()
            {
                output.push(' ');
                output.push_str(scheme);
                output.push_str(" [secret redacted]");
            } else {
                output.push_str(" [secret redacted]");
            }
            rest = &rest[value_end..];
            lower_rest = &lower_rest[value_end..];
        } else {
            let value_start = index + "bearer ".len();
            let value_end = lower_rest[value_start..]
                .find(|character: char| {
                    character.is_whitespace() || [',', '"', '\'', '}', ')'].contains(&character)
                })
                .map(|offset| value_start + offset)
                .unwrap_or(rest.len());
            output.push_str(&rest[index..value_start]);
            output.push_str("[secret redacted]");
            rest = &rest[value_end..];
            lower_rest = &lower_rest[value_end..];
        }
    }
    output.push_str(rest);
    output
}

fn redact_credential_assignments(detail: &str) -> String {
    let lower = detail.to_ascii_lowercase();
    let mut output = String::with_capacity(detail.len());
    let mut cursor = 0;
    while cursor < detail.len() {
        let candidate = SENSITIVE_CREDENTIAL_LABELS
            .iter()
            .filter_map(|label| {
                lower[cursor..]
                    .find(label)
                    .map(|offset| (cursor + offset, *label))
            })
            .min_by_key(|(index, _)| *index);
        let Some((start, label)) = candidate else {
            output.push_str(&detail[cursor..]);
            break;
        };
        let before_is_label = start > 0
            && lower[..start]
                .chars()
                .next_back()
                .is_some_and(|character| character.is_ascii_alphanumeric() || character == '_');
        let after_label = start + label.len();
        if before_is_label {
            output.push_str(&detail[cursor..after_label]);
            cursor = after_label;
            continue;
        }
        let mut value_start = after_label;
        while lower
            .as_bytes()
            .get(value_start)
            .is_some_and(u8::is_ascii_whitespace)
        {
            value_start += 1;
        }
        if !matches!(lower.as_bytes().get(value_start), Some(b':' | b'=')) {
            output.push_str(&detail[cursor..after_label]);
            cursor = after_label;
            continue;
        }
        value_start += 1;
        while lower
            .as_bytes()
            .get(value_start)
            .is_some_and(u8::is_ascii_whitespace)
        {
            value_start += 1;
        }
        const REDACTED_MARKER: &str = "[secret redacted]";
        if detail[value_start..].starts_with(REDACTED_MARKER) {
            let value_end = value_start + REDACTED_MARKER.len();
            output.push_str(&detail[cursor..value_end]);
            cursor = value_end;
            continue;
        }
        let quote = lower
            .as_bytes()
            .get(value_start)
            .copied()
            .filter(|byte| matches!(byte, b'"' | b'\''));
        if quote.is_some() {
            value_start += 1;
        }
        let value_end = if let Some(quote) = quote {
            detail[value_start..]
                .find(char::from(quote))
                .map(|offset| value_start + offset)
                .unwrap_or(detail.len())
        } else {
            detail[value_start..]
                .find(|character: char| {
                    character.is_whitespace() || [',', ';', '}', ')'].contains(&character)
                })
                .map(|offset| value_start + offset)
                .unwrap_or(detail.len())
        };
        output.push_str(&detail[cursor..value_start]);
        output.push_str("[secret redacted]");
        cursor = value_end;
    }
    output
}

pub(crate) fn sanitize_diagnostic(detail: &str) -> String {
    sanitize_diagnostic_with_secret(detail, None)
}

pub(crate) fn sanitize_diagnostic_with_secret(detail: &str, secret: Option<&str>) -> String {
    let detail = secret.filter(|secret| !secret.is_empty()).map_or_else(
        || detail.to_owned(),
        |secret| redact_exact_secret(detail, secret),
    );
    redact_embedded_urls(&redact_credential_assignments(
        &redact_authorization_values(&detail),
    ))
}

fn redact_exact_secret(detail: &str, secret: &str) -> String {
    if secret.is_empty() {
        detail.to_owned()
    } else {
        detail.replace(secret, "[secret redacted]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_authorization_and_embedded_credentials_without_hiding_context() {
        assert_eq!(
            sanitize_diagnostic_with_secret("token fixture-token remains", Some("fixture-token")),
            "token [secret redacted] remains"
        );
        assert_eq!(
            redact_authorization_values("Authorization: Basic hidden\nBearer abc.def still safe"),
            "Authorization: Basic [secret redacted]\nBearer [secret redacted] still safe"
        );
        assert_eq!(
            sanitize_diagnostic(
                r#"failed password="hidden value" api_key=abc123 path=/srv/app ordinary https://example.invalid/a?keep=yes"#
            ),
            r#"failed password="[secret redacted]" api_key=[secret redacted] path=/srv/app ordinary https://example.invalid/a?keep=yes"#
        );
    }

    #[test]
    fn sanitizes_sensitive_url_values_and_preserves_nonsecret_query_data() {
        assert_eq!(
            redact_embedded_urls(
                "failed https://example.invalid/path?keep=visible&access_token=hidden, then retry"
            ),
            "failed https://example.invalid/path?keep=visible&access_token=%5Bsecret+redacted%5D, then retry"
        );
        assert_eq!(
            redact_embedded_urls(
                "failed https://cdn.example.invalid/file?keep=visible&hm=signature, retry"
            ),
            "failed https://cdn.example.invalid/file?keep=visible&hm=%5Bsecret+redacted%5D, retry"
        );
        assert_eq!(
            redact_url_secrets("https://user:password@example.invalid/path"),
            "https://%5Bsecret%20redacted%5D:%5Bsecret%20redacted%5D@example.invalid/path"
        );
    }

    #[test]
    fn sanitizes_native_error_while_preserving_status_code_and_message() {
        assert_eq!(
            sanitize_diagnostic_with_secret(
                "status 503; -32001: provider rejected api_key=fixture-secret; retry later",
                Some("fixture-secret"),
            ),
            "status 503; -32001: provider rejected api_key=[secret redacted]; retry later"
        );
    }

    #[test]
    fn sanitizing_an_exact_secret_replacement_is_idempotent_for_assignments() {
        assert_eq!(
            sanitize_diagnostic_with_secret(
                "status 503; provider rejected api_key=fixture-secret; retry later",
                Some("fixture-secret"),
            ),
            "status 503; provider rejected api_key=[secret redacted]; retry later"
        );
    }
}
