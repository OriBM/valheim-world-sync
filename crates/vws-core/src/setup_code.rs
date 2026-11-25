//! The setup code Ori sends each friend once (PLAN.md 4.1):
//! `VWS1.` followed by base64url of `{"owner": "...", "repo": "...", "token": "..."}`.
//! It is a secret: it is never logged and never shown again after entry.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

const PREFIX: &str = "VWS1.";

/// A string that never prints itself.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Secret(s.into())
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(***)")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupCode {
    pub owner: String,
    pub repo: String,
    pub token: Secret,
}

#[derive(Serialize, Deserialize)]
struct Wire {
    owner: String,
    repo: String,
    token: String,
}

fn damaged() -> Error {
    Error::Invalid("the setup code is damaged; copy it again from the message".into())
}

pub fn parse(code: &str) -> Result<SetupCode> {
    // Chat apps sometimes wrap long text; ignore all whitespace.
    let code: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    let body = code
        .strip_prefix(PREFIX)
        .ok_or_else(|| Error::Invalid("this is not a setup code (it starts with VWS1.)".into()))?;
    let raw = URL_SAFE_NO_PAD
        .decode(body.trim_end_matches('='))
        .map_err(|_| damaged())?;
    let w: Wire = serde_json::from_slice(&raw).map_err(|_| damaged())?;
    validate_repo_part(&w.owner)?;
    validate_repo_part(&w.repo)?;
    let token = w.token.trim();
    if token.is_empty() || token.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(damaged());
    }
    Ok(SetupCode {
        owner: w.owner,
        repo: w.repo,
        token: Secret(token.to_string()),
    })
}

pub fn encode(owner: &str, repo: &str, token: &str) -> Result<String> {
    validate_repo_part(owner)?;
    validate_repo_part(repo)?;
    let wire = Wire {
        owner: owner.into(),
        repo: repo.into(),
        token: token.trim().into(),
    };
    Ok(format!(
        "{PREFIX}{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&wire)?)
    ))
}

/// GitHub owner and repository names: letters, digits, `-`, `_`, `.`.
pub fn validate_repo_part(s: &str) -> Result<()> {
    let ok = !s.is_empty()
        && s.len() <= 100
        && s != "."
        && s != ".."
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if ok {
        Ok(())
    } else {
        Err(Error::Invalid(format!("not a GitHub owner or repository name: {s:?}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_tolerates_line_breaks() {
        let code = encode("ori", "valheim-world-data", "github_pat_TEST").unwrap();
        assert!(code.starts_with("VWS1."));
        let (a, b) = code.split_at(20);
        let parsed = parse(&format!("  {a}\n{b} ")).unwrap();
        assert_eq!(parsed.owner, "ori");
        assert_eq!(parsed.repo, "valheim-world-data");
        assert_eq!(parsed.token.expose(), "github_pat_TEST");
        assert_eq!(format!("{:?}", parsed.token), "Secret(***)");
        assert!(!format!("{parsed:?}").contains("github_pat_TEST"));
    }

    #[test]
    fn rejects_bad_codes() {
        assert!(parse("hello").is_err());
        assert!(parse("VWS1.!!!").is_err());
        let evil = format!(
            "VWS1.{}",
            URL_SAFE_NO_PAD.encode(br#"{"owner":"../x","repo":"r","token":"t"}"#)
        );
        assert!(parse(&evil).is_err());
    }
}
