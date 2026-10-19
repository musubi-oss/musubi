//! Resource scopes (competitive P0-55) shared by down-scoped player tokens and scoped server
//! keys. A scope is `<area>`, `<area>:read` or `<area>:write` where the area is the first path
//! segment after `/v1/` (`storage`, `friends`, `chat`, `leaderboards`, `wallet`, `store`, …) or
//! `realtime` for the socket; `*` means everything. Reads are GET / HEAD, everything else is a
//! write. An empty scope list means "not scoped" and is never restricted here.

use musubi_core::{Error, Result};

pub const MAX_SCOPES: usize = 32;

/// Area a request path belongs to.
pub fn area_of(path: &str) -> &str {
    if path == "/ws" || path.starts_with("/ws/") || path.starts_with("/v1/realtime") {
        return "realtime";
    }
    let rest = path
        .strip_prefix("/v1/")
        .unwrap_or(path.trim_start_matches('/'));
    let rest = rest.strip_prefix("server/").unwrap_or(rest);
    let rest = rest.strip_prefix("admin/").unwrap_or(rest);
    rest.split('/').next().unwrap_or("")
}

fn valid_area(a: &str) -> bool {
    !a.is_empty()
        && a.len() <= 64
        && a.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

pub fn validate(scopes: &[String]) -> Result<()> {
    if scopes.len() > MAX_SCOPES {
        return Err(Error::InvalidArgument(format!(
            "at most {MAX_SCOPES} scopes"
        )));
    }
    for s in scopes {
        if s == "*" {
            continue;
        }
        let (area, mode) = s.split_once(':').unwrap_or((s.as_str(), ""));
        if !valid_area(area) || !matches!(mode, "" | "read" | "write") {
            return Err(Error::InvalidArgument(format!(
                "scope '{s}': expected <area>, <area>:read, <area>:write or *"
            )));
        }
    }
    Ok(())
}

/// May a holder of `scopes` perform `method` on `path`? (Empty = unrestricted.)
pub fn allows(scopes: &[String], method: &str, path: &str) -> bool {
    if scopes.is_empty() || path == "/v1/auth/token/scope" {
        // narrowing a scoped token further is always allowed
        return true;
    }
    let read = matches!(method, "GET" | "HEAD");
    let area = area_of(path);
    scopes.iter().any(|s| {
        if s == "*" {
            return true;
        }
        let (a, mode) = s.split_once(':').unwrap_or((s.as_str(), ""));
        a == area && (mode.is_empty() || (mode == "read" && read) || (mode == "write" && !read))
    })
}

/// Is `narrower` a subset of what `wider` permits? (Empty `wider` = everything.)
pub fn within(narrower: &[String], wider: &[String]) -> bool {
    if wider.is_empty() || wider.iter().any(|s| s == "*") {
        return true;
    }
    narrower.iter().all(|n| {
        if n == "*" {
            return false;
        }
        let (na, nm) = n.split_once(':').unwrap_or((n.as_str(), ""));
        wider.iter().any(|w| {
            let (wa, wm) = w.split_once(':').unwrap_or((w.as_str(), ""));
            wa == na && (wm.is_empty() || wm == nm)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn areas_and_modes() {
        assert_eq!(area_of("/v1/leaderboards/x/records"), "leaderboards");
        assert_eq!(area_of("/v1/server/wallet/adjust"), "wallet");
        assert_eq!(area_of("/ws"), "realtime");
        assert!(allows(&[], "DELETE", "/v1/anything"));
        let s = v(&["leaderboards:read", "storage"]);
        assert!(allows(&s, "GET", "/v1/leaderboards/weekly"));
        assert!(!allows(&s, "POST", "/v1/leaderboards/weekly/records"));
        assert!(allows(&s, "PUT", "/v1/storage/c/k"));
        assert!(!allows(&s, "GET", "/v1/wallet"));
        assert!(!allows(&s, "GET", "/ws"));
        assert!(allows(&v(&["*"]), "POST", "/v1/wallet"));
    }

    #[test]
    fn validation_and_subsets() {
        assert!(validate(&v(&["storage", "chat:write", "*"])).is_ok());
        assert!(validate(&v(&["chat:delete"])).is_err());
        assert!(validate(&v(&["bad area!"])).is_err());
        assert!(within(&v(&["storage:read"]), &v(&["storage"])));
        assert!(within(&v(&["storage:read"]), &[]));
        assert!(!within(&v(&["storage"]), &v(&["storage:read"])));
        assert!(!within(&v(&["wallet"]), &v(&["storage"])));
        assert!(!within(&v(&["*"]), &v(&["storage"])));
    }
}
