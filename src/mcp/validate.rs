use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};

use url::{Host, Url};

use crate::error::{Result, VaultError};
use crate::ingest;

/// Validate an HTTP(S) source URL and reject SSRF targets (loopback, private, link-local,
/// CGNAT, unspecified, multicast, broadcast, and local hostnames).
pub fn validate_web_url(raw: &str) -> Result<Url> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(VaultError::Validation(
            "origin URL must not be empty".into(),
        ));
    }

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

    let Some(host) = parsed.host() else {
        return Err(VaultError::Validation(format!(
            "origin URL '{trimmed}' must include a valid host"
        )));
    };

    match host {
        Host::Ipv4(ip) => {
            if is_blocked_ipv4(ip) {
                return Err(VaultError::Validation(format!(
                    "origin URL '{trimmed}' targets blocked IPv4 address {ip}"
                )));
            }
        }
        Host::Ipv6(ip) => {
            if is_blocked_ipv6(ip) {
                return Err(VaultError::Validation(format!(
                    "origin URL '{trimmed}' targets blocked IPv6 address {ip}"
                )));
            }
        }
        Host::Domain(domain) => validate_domain_host(trimmed, domain)?,
    }

    Ok(parsed)
}

/// Validate that `raw` refers to an existing directory and return its canonical path.
pub fn validate_local_dir(raw: &str) -> Result<PathBuf> {
    ingest::canonicalize_local_dir(Path::new(raw.trim()))
}

fn validate_domain_host(raw: &str, domain: &str) -> Result<()> {
    let normalized = domain.trim_end_matches('.').to_ascii_lowercase();
    if normalized.is_empty() {
        return Err(VaultError::Validation(format!(
            "origin URL '{raw}' must include a valid host"
        )));
    }

    if normalized == "localhost"
        || normalized == "local"
        || normalized.ends_with(".localhost")
        || normalized.ends_with(".local")
    {
        return Err(VaultError::Validation(format!(
            "origin URL '{raw}' targets blocked local hostname '{domain}'"
        )));
    }

    let unbracketed = normalized.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = unbracketed.parse::<IpAddr>() {
        if is_blocked_ip(ip) {
            return Err(VaultError::Validation(format!(
                "origin URL '{raw}' targets blocked IP address {ip}"
            )));
        }
    }

    Ok(())
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

    #[test]
    fn blocks_ssrf_targets() {
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
            let err = validate_web_url(target)
                .expect_err(&format!("expected SSRF/scheme rejection for {target}"));
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
            let parsed =
                validate_web_url(target).unwrap_or_else(|e| panic!("expected {target} ok: {e}"));
            assert!(matches!(parsed.scheme(), "http" | "https"));
        }

        // Verify local directory validation delegates to canonicalize_local_dir.
        let dir = tempdir().expect("tempdir");
        let file_path = dir.path().join("note.md");
        std::fs::write(&file_path, "# Hi\n").expect("write note");
        let canon = validate_local_dir(&dir.path().to_string_lossy()).expect("valid dir");
        assert!(canon.is_dir());
        assert!(validate_local_dir(&file_path.to_string_lossy()).is_err());
        assert!(validate_local_dir("/nonexistent/path/web-vault-check").is_err());
    }
}
