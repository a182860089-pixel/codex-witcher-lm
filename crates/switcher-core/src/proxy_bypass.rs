//! Loopback bypass helpers for HTTP clients that honor `NO_PROXY`.
//!
//! Codex's Rust client follows `HTTP_PROXY` and does not honor Windows WinINET
//! `127.*` override rules. Clash and similar local proxies then intercept
//! `http://127.0.0.1:15722` and return 502. Explicit loopback hosts in
//! `NO_PROXY` skip that interception.

pub const LOOPBACK_NO_PROXY_HOSTS: &[&str] = &["127.0.0.1", "localhost", "::1", "[::1]"];
pub const NO_PROXY_MAX_CHARS: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SanitizedNoProxy {
    pub value: String,
    pub truncated: bool,
    pub original: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserNoProxyPersistPlan {
    pub process_value: String,
    pub persist_user_no_proxy: String,
    pub delete_user_no_proxy_alt: bool,
    pub backup_original: Option<String>,
    pub persist_changed: bool,
}

pub fn merge_no_proxy(existing: Option<&str>) -> String {
    sanitize_no_proxy(existing).value
}

pub fn loopback_no_proxy_value() -> String {
    LOOPBACK_NO_PROXY_HOSTS.join(",")
}

pub fn select_no_proxy_source<'a>(
    user_no_proxy: Option<&'a str>,
    user_no_proxy_alt: Option<&'a str>,
    process: Option<&'a str>,
) -> Option<&'a str> {
    [user_no_proxy, user_no_proxy_alt, process]
        .into_iter()
        .find_map(|value| value.map(str::trim).filter(|part| !part.is_empty()))
}

pub fn sanitize_no_proxy(existing: Option<&str>) -> SanitizedNoProxy {
    let raw = existing.unwrap_or("").trim();
    let original_too_long = raw.len() > NO_PROXY_MAX_CHARS;
    let mut parts = parse_no_proxy_parts(Some(raw));
    for host in LOOPBACK_NO_PROXY_HOSTS {
        if !parts.iter().any(|part| part.eq_ignore_ascii_case(host)) {
            parts.push((*host).to_string());
        }
    }
    let merged = parts.join(",");
    if original_too_long || merged.len() > NO_PROXY_MAX_CHARS {
        SanitizedNoProxy {
            value: loopback_no_proxy_value(),
            truncated: true,
            original: (!raw.is_empty()).then(|| raw.to_string()),
        }
    } else {
        SanitizedNoProxy {
            value: merged,
            truncated: false,
            original: None,
        }
    }
}

pub fn should_delete_duplicate_no_proxy(existing_lower: Option<&str>, persisted: &str) -> bool {
    let Some(existing) = existing_lower
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return false;
    };
    existing.len() > NO_PROXY_MAX_CHARS || existing.eq_ignore_ascii_case(persisted)
}

pub fn plan_user_no_proxy_persist(
    user_no_proxy: Option<&str>,
    user_no_proxy_alt: Option<&str>,
    process: Option<&str>,
) -> UserNoProxyPersistPlan {
    let source = select_no_proxy_source(user_no_proxy, user_no_proxy_alt, process);
    let sanitized = sanitize_no_proxy(source);
    let persist = sanitized.value.clone();
    let existing_upper = user_no_proxy.unwrap_or("").trim();
    let delete_user_no_proxy_alt = should_delete_duplicate_no_proxy(user_no_proxy_alt, &persist);
    let persist_changed = existing_upper != persist.as_str() || delete_user_no_proxy_alt;
    UserNoProxyPersistPlan {
        process_value: persist.clone(),
        persist_user_no_proxy: persist,
        delete_user_no_proxy_alt,
        backup_original: sanitized.original,
        persist_changed,
    }
}

pub fn no_proxy_covers_loopback(existing: Option<&str>) -> bool {
    let parts = parse_no_proxy_parts(existing);
    LOOPBACK_NO_PROXY_HOSTS
        .iter()
        .all(|host| parts.iter().any(|part| part.eq_ignore_ascii_case(host)))
}

fn parse_no_proxy_parts(existing: Option<&str>) -> Vec<String> {
    let mut parts = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for part in existing
        .unwrap_or("")
        .split(|character: char| matches!(character, ',' | ';' | ' '))
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        if seen.insert(part.to_ascii_lowercase()) {
            parts.push(part.to_string());
        }
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_adds_loopback_hosts_to_an_empty_value() {
        assert_eq!(merge_no_proxy(None), "127.0.0.1,localhost,::1,[::1]");
        assert_eq!(merge_no_proxy(Some("")), "127.0.0.1,localhost,::1,[::1]");
        assert!(!no_proxy_covers_loopback(None));
        assert!(!no_proxy_covers_loopback(Some("")));
    }

    #[test]
    fn merge_preserves_existing_hosts_and_is_idempotent() {
        let merged = merge_no_proxy(Some("example.com, 10.0.0.0/8"));
        assert_eq!(
            merged,
            "example.com,10.0.0.0/8,127.0.0.1,localhost,::1,[::1]"
        );
        assert_eq!(merge_no_proxy(Some(&merged)), merged);
        assert!(no_proxy_covers_loopback(Some(&merged)));
    }

    #[test]
    fn wininet_wildcard_patterns_do_not_count_as_reqwest_bypass() {
        assert!(!no_proxy_covers_loopback(Some("localhost;127.*;<local>")));
        let merged = merge_no_proxy(Some("localhost;127.*;<local>"));
        assert!(merged.contains("127.0.0.1"));
        assert!(merged.contains("::1"));
        assert!(merged.contains("[::1]"));
        assert!(no_proxy_covers_loopback(Some(&merged)));
    }

    #[test]
    fn merge_is_case_insensitive_and_accepts_semicolon_lists() {
        let existing = "LocalHost,127.0.0.1;::1,[::1]";
        assert!(no_proxy_covers_loopback(Some(existing)));
        assert_eq!(
            merge_no_proxy(Some(existing)),
            "LocalHost,127.0.0.1,::1,[::1]"
        );
    }

    #[test]
    fn merge_deduplicates_mixed_case_and_separators() {
        assert_eq!(
            merge_no_proxy(Some("Example.com, example.com; EXAMPLE.COM 10.0.0.0/8")),
            "Example.com,10.0.0.0/8,127.0.0.1,localhost,::1,[::1]"
        );
    }

    #[test]
    fn sanitize_truncates_oversized_input_to_loopback() {
        let huge = "example.com,".repeat(140_000);
        assert!(huge.len() > 1_500_000);
        let sanitized = sanitize_no_proxy(Some(&huge));
        assert!(sanitized.truncated);
        assert_eq!(sanitized.value, loopback_no_proxy_value());
        assert_eq!(sanitized.original.as_deref(), Some(huge.as_str()));
        assert_eq!(merge_no_proxy(Some(&sanitized.value)), sanitized.value);
    }

    #[test]
    fn select_no_proxy_source_does_not_concatenate() {
        let selected = select_no_proxy_source(
            Some("user-upper"),
            Some("user-lower"),
            Some("process-value"),
        );
        assert_eq!(selected, Some("user-upper"));
        assert_eq!(
            select_no_proxy_source(None, Some(" user-lower "), Some("process")),
            Some("user-lower")
        );
        assert_eq!(
            select_no_proxy_source(None, None, Some("process-value")),
            Some("process-value")
        );
        assert_eq!(select_no_proxy_source(Some(""), Some("  "), None), None);
    }

    #[test]
    fn duplicate_or_bloated_no_proxy_is_deleted() {
        let persisted = loopback_no_proxy_value();
        assert!(should_delete_duplicate_no_proxy(
            Some(&persisted),
            &persisted
        ));
        assert!(should_delete_duplicate_no_proxy(
            Some(&"x".repeat(NO_PROXY_MAX_CHARS + 1)),
            &persisted
        ));
        assert!(!should_delete_duplicate_no_proxy(
            Some("other.example"),
            &persisted
        ));
        assert!(!should_delete_duplicate_no_proxy(None, &persisted));
    }

    #[test]
    fn persist_plan_uses_one_source_and_truncates_without_concatenating() {
        let huge = "example.com,".repeat(80_000);
        let plan = plan_user_no_proxy_persist(Some(&huge), Some(&huge), Some(&huge));
        assert_eq!(plan.process_value, loopback_no_proxy_value());
        assert_eq!(plan.persist_user_no_proxy, loopback_no_proxy_value());
        assert!(plan.delete_user_no_proxy_alt);
        assert_eq!(plan.backup_original.as_deref(), Some(huge.as_str()));
        assert!(plan.persist_changed);

        let loopback = loopback_no_proxy_value();
        let again = plan_user_no_proxy_persist(Some(&loopback), None, Some(&loopback));
        assert!(!again.persist_changed);
        assert!(!again.delete_user_no_proxy_alt);
        assert_eq!(again.process_value, loopback);

        let from_alt = plan_user_no_proxy_persist(None, Some("other.example"), None);
        assert_eq!(
            from_alt.persist_user_no_proxy,
            "other.example,127.0.0.1,localhost,::1,[::1]"
        );
        assert!(!from_alt.delete_user_no_proxy_alt);
        assert!(from_alt.persist_changed);
    }
}
