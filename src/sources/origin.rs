//! Origin classification and the Private-origin rule.
//!
//! This is the only place that decides whether an Origin is a Private origin.
//! The Source lifecycle applies it on `add` and before every web Sync; the
//! crawler's redirect guard reuses [`is_private_host`].

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::Path;

use url::{Host, Url};

use crate::config::ALLOW_PRIVATE_ORIGINS_ENV;
use crate::error::{Result, VaultError};
use crate::ingest::{canonicalize_local_dir, SourceKind};

/// Classify a raw Origin as a web URL or a local Markdown folder and normalize it.
///
/// Anything with a scheme (`://`, or `http:`, `https:`, `file:`, `ftp:`) is parsed as a
/// URL and must be `http(s)://` with a host and no credentials; everything else must be
/// an existing directory, returned canonicalized.
pub(crate) fn classify_origin(raw_origin: &str) -> Result<(SourceKind, String)> {
    let trimmed = raw_origin.trim();
    if trimmed.is_empty() {
        return Err(VaultError::Validation(
            "origin must not be empty; pass an http(s):// URL or local directory path".into(),
        ));
    }

    if looks_like_url(trimmed) {
        return Ok((SourceKind::Web, parse_web_origin(trimmed)?.into()));
    }

    let canon = canonicalize_local_dir(Path::new(trimmed))?;
    let canon_str = canon.to_str().ok_or_else(|| {
        VaultError::Validation(format!(
            "local origin directory '{}' contains non-UTF-8 characters",
            canon.display()
        ))
    })?;
    Ok((SourceKind::Local, canon_str.to_string()))
}

/// Refuse a web Origin that is a Private origin unless the vault allows them.
pub(crate) fn check_private_origin(origin: &str, allow_private_origins: bool) -> Result<()> {
    let url = parse_web_origin(origin)?;
    if !allow_private_origins && is_private_host(&url) {
        return Err(VaultError::Validation(format!(
            "origin URL '{origin}' is a private origin (loopback, private network, link-local \
             or local hostname); set {ALLOW_PRIVATE_ORIGINS_ENV}=1 to allow it"
        )));
    }
    Ok(())
}

/// Whether `url`'s host is a Private origin: loopback, private, link-local, CGNAT,
/// unspecified, multicast, broadcast, or a local hostname.
pub(crate) fn is_private_host(url: &Url) -> bool {
    match url.host() {
        Some(Host::Ipv4(ip)) => is_blocked_ipv4(ip),
        Some(Host::Ipv6(ip)) => is_blocked_ipv6(ip),
        Some(Host::Domain(domain)) => is_private_domain(domain),
        None => false,
    }
}

fn parse_web_origin(raw: &str) -> Result<Url> {
    let trimmed = raw.trim();
    let parsed = Url::parse(trimmed)
        .map_err(|err| VaultError::Validation(format!("invalid origin URL '{trimmed}': {err}")))?;

    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(VaultError::Validation(format!(
            "origin URL '{trimmed}' must use http:// or https:// (rejected scheme '{}')",
            parsed.scheme()
        )));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(VaultError::Validation(
            "origin URL must not contain embedded user credentials".into(),
        ));
    }
    match parsed.host_str() {
        Some(host) if !host.trim_end_matches('.').is_empty() => Ok(parsed),
        _ => Err(VaultError::Validation(format!(
            "origin URL '{trimmed}' must include a valid host"
        ))),
    }
}

fn looks_like_url(origin: &str) -> bool {
    let lower = origin.to_ascii_lowercase();
    lower.contains("://")
        || lower.starts_with("http:")
        || lower.starts_with("https:")
        || lower.starts_with("file:")
        || lower.starts_with("ftp:")
}

fn is_private_domain(domain: &str) -> bool {
    let normalized = domain.trim_end_matches('.').to_ascii_lowercase();
    if normalized == "localhost"
        || normalized == "local"
        || normalized.ends_with(".localhost")
        || normalized.ends_with(".local")
    {
        return true;
    }
    let unbracketed = normalized.trim_start_matches('[').trim_end_matches(']');
    unbracketed.parse::<IpAddr>().is_ok_and(is_blocked_ip)
}

fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_blocked_ipv4(v4),
        IpAddr::V6(v6) => is_blocked_ipv6(v6),
    }
}

fn is_blocked_ipv4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    let is_this_network = octets[0] == 0;
    let is_cgnat = octets[0] == 100 && (octets[1] & 0b1100_0000) == 64;

    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast()
        || is_this_network
        || is_cgnat
}

fn is_blocked_ipv6(ip: Ipv6Addr) -> bool {
    let seg = ip.segments();
    let is_unique_local = (seg[0] & 0xfe00) == 0xfc00;
    let is_link_local = (seg[0] & 0xffc0) == 0xfe80;
    let is_ipv4_mapped = seg[0..6] == [0, 0, 0, 0, 0, 0xffff];
    let is_ipv4_compat = seg[0..6] == [0, 0, 0, 0, 0, 0];

    let blocked_v4_compat = is_ipv4_compat && {
        let mapped = Ipv4Addr::new(
            (seg[6] >> 8) as u8,
            (seg[6] & 0xff) as u8,
            (seg[7] >> 8) as u8,
            (seg[7] & 0xff) as u8,
        );
        is_blocked_ipv4(mapped)
    };

    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || is_unique_local
        || is_link_local
        || is_ipv4_mapped
        || blocked_v4_compat
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn validate_web(raw: &str) -> Result<String> {
        match classify_origin(raw)? {
            (SourceKind::Web, origin) => {
                check_private_origin(&origin, false)?;
                Ok(origin)
            }
            (SourceKind::Local, origin) => Err(VaultError::Validation(format!(
                "expected a web origin, got local folder {origin}"
            ))),
        }
    }

    #[test]
    fn blocks_private_origins_and_bad_schemes() {
        let blocked = [
            "",
            "file:///etc/passwd",
            "ftp://example.com/archive.tar.gz",
            "javascript:alert(1)",
            "https://user:pass@docs.example.com",
            "http://127.0.0.1",
            "http://127.0.0.1:8080/docs",
            "http://127.1/docs",
            "http://localhost",
            "http://localhost.",
            "http://localhost:3000",
            "http://sub.localhost/docs",
            "http://printer.local/docs",
            "http://169.254.169.254/latest/meta-data/",
            "http://10.0.0.1",
            "http://172.16.0.1",
            "http://172.31.255.255",
            "http://192.168.1.1",
            "http://100.64.0.1",
            "http://100.127.255.254",
            "http://0.0.0.0",
            "http://0.1.2.3",
            "http://224.0.0.1",
            "http://255.255.255.255",
            "http://[::1]",
            "http://[::1]:4317",
            "http://[::]",
            "http://[fc00::1]",
            "http://[fd12:3456::1]",
            "http://[fe80::1]",
            "http://[ff02::1]",
            "http://[::ffff:127.0.0.1]",
            "http://[::ffff:169.254.169.254]",
            "http://[::ffff:8.8.8.8]",
            "http://[::127.0.0.1]",
        ];

        for target in blocked {
            let err = validate_web(target).expect_err(&format!(
                "expected private-origin/scheme rejection for {target}"
            ));
            assert!(
                matches!(err, VaultError::Validation(_)),
                "expected Validation error for {target}, got {err:?}"
            );
        }

        let allowed = [
            "https://docs.example.com",
            "https://docs.example.com/guide/intro",
            "http://172.15.0.1/docs",
            "http://172.32.0.1/docs",
            "https://[2606:4700:4700::1111]/docs",
        ];

        for target in allowed {
            let origin =
                validate_web(target).unwrap_or_else(|e| panic!("expected {target} ok: {e}"));
            assert!(origin.starts_with("http"));
        }
    }

    #[test]
    fn private_origins_pass_when_allowed() {
        for target in [
            "http://127.0.0.1:8080/docs",
            "http://printer.local/",
            "http://[::1]/",
        ] {
            check_private_origin(target, true)
                .unwrap_or_else(|e| panic!("expected {target} allowed: {e}"));
        }
    }

    #[test]
    fn classifies_local_directories() {
        let dir = tempdir().expect("tempdir");
        let file_path = dir.path().join("note.md");
        std::fs::write(&file_path, "# Hi\n").expect("write note");

        let (kind, canon) = classify_origin(&dir.path().to_string_lossy()).expect("valid dir");
        assert_eq!(kind, SourceKind::Local);
        assert!(Path::new(&canon).is_dir());
        assert!(classify_origin(&file_path.to_string_lossy()).is_err());
        assert!(classify_origin("/nonexistent/path/web-vault-check").is_err());
    }
}
