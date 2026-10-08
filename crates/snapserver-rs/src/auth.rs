//! Control-API authentication: configured users, `Server.Authenticate`
//! credentials (Basic, Plain, Bearer) and the JWT tokens `Server.GetToken`
//! issues.

use anyhow::Result;
use base64::Engine;
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use subtle::{Choice, ConstantTimeEq};

/// JWT claims.
#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    /// Subject (the username).
    sub: String,
    /// Expiration (Unix timestamp).
    exp: u64,
}

/// A control-API user, given as `<name>:<password>` (`user = ...` under
/// `[auth]`, or `--auth-user`). The name ends at the first `:`, so the
/// password may contain `:`.
#[derive(Clone, PartialEq, Eq)]
pub struct AuthUser {
    /// Username.
    pub name: String,
    /// Plaintext password.
    pub password: String,
}

impl std::str::FromStr for AuthUser {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.split_once(':') {
            Some((name, password)) if !name.is_empty() && !password.is_empty() => Ok(Self {
                name: name.to_string(),
                password: password.to_string(),
            }),
            _ => Err("expected <name>:<password> with a non-empty name and password".into()),
        }
    }
}

impl std::fmt::Debug for AuthUser {
    /// Never prints the password.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthUser")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// Auth configuration.
///
/// `Default` is disabled with an **empty** secret and no users. A real secret
/// must be supplied explicitly (config/CLI) before authentication is enabled —
/// there is deliberately no built-in default secret, since a shipped constant
/// would be public and forgeable by anyone.
#[derive(Debug, Clone, Default)]
pub struct AuthConfig {
    /// Whether authentication is required.
    pub enabled: bool,
    /// Secret key for JWT signing/validation.
    pub secret: String,
    /// Users that may authenticate.
    pub users: Vec<AuthUser>,
}

impl AuthConfig {
    /// Reject an inconsistent configuration: enabled without a secret, or
    /// without a user to log in as.
    ///
    /// Call this at startup and refuse to run on error — an enabled-but-
    /// secretless config would otherwise sign tokens with an empty key.
    pub fn validate(&self) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        if self.secret.trim().is_empty() {
            anyhow::bail!(
                "authentication is enabled but no secret is configured \
                 (set [auth] secret in the config file or pass --auth-secret)"
            );
        }
        if self.users.is_empty() {
            anyhow::bail!(
                "authentication is enabled but no user is configured \
                 (add [auth] user = <name>:<password> to the config file or \
                 pass --auth-user <name>:<password>)"
            );
        }
        Ok(())
    }
}

/// Why a credential check failed.
#[derive(Debug, PartialEq, Eq)]
pub enum AuthFailure {
    /// Unknown user, wrong password, bad or expired token, malformed
    /// credentials. Deliberately carries no detail, so a caller cannot tell
    /// an unknown user from a wrong password.
    Unauthorized,
    /// The scheme is not Basic, Plain or Bearer.
    UnsupportedScheme,
}

/// Check `name`/`password` against the configured users.
///
/// Compares every user in constant time and does not stop at a match, so the
/// time taken does not reveal whether the user exists or how much of the
/// password matched (only the lengths compared can show).
pub fn verify_credentials(config: &AuthConfig, name: &str, password: &str) -> bool {
    let mut found = Choice::from(0);
    for user in &config.users {
        let name_ok = user.name.as_bytes().ct_eq(name.as_bytes());
        let password_ok = user.password.as_bytes().ct_eq(password.as_bytes());
        found |= name_ok & password_ok;
    }
    found.into()
}

/// Check `name:password` (split at the first `:`); returns the username.
fn verify_pair(config: &AuthConfig, pair: &str) -> Result<String, AuthFailure> {
    let (name, password) = pair.split_once(':').ok_or(AuthFailure::Unauthorized)?;
    if verify_credentials(config, name, password) {
        Ok(name.to_string())
    } else {
        Err(AuthFailure::Unauthorized)
    }
}

/// Check the credentials of `Server.Authenticate` (or an HTTP `Authorization`
/// header): `Basic` takes `base64(name:password)`, `Plain` takes
/// `name:password` and `Bearer` takes a token from `Server.GetToken`. The
/// scheme is matched case-insensitively. Returns the authenticated username.
pub fn authenticate(config: &AuthConfig, scheme: &str, param: &str) -> Result<String, AuthFailure> {
    if scheme.eq_ignore_ascii_case("basic") {
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(param.trim())
            .map_err(|_| AuthFailure::Unauthorized)?;
        let pair = String::from_utf8(decoded).map_err(|_| AuthFailure::Unauthorized)?;
        verify_pair(config, &pair)
    } else if scheme.eq_ignore_ascii_case("plain") {
        verify_pair(config, param)
    } else if scheme.eq_ignore_ascii_case("bearer") {
        validate_token(config, param.trim()).map_err(|_| AuthFailure::Unauthorized)
    } else {
        Err(AuthFailure::UnsupportedScheme)
    }
}

/// Validate an HTTP `Authorization` header (`Bearer <token>` or
/// `Basic <base64>`). Returns Ok(subject) or Err.
pub fn validate_authorization(config: &AuthConfig, header: Option<&str>) -> Result<String> {
    if !config.enabled {
        return Ok("anonymous".into());
    }
    let header = header.ok_or_else(|| anyhow::anyhow!("missing Authorization header"))?;
    let (scheme, param) = header
        .trim()
        .split_once(' ')
        .ok_or_else(|| anyhow::anyhow!("malformed Authorization header"))?;
    if !scheme.eq_ignore_ascii_case("bearer") && !scheme.eq_ignore_ascii_case("basic") {
        anyhow::bail!("expected a Bearer or Basic Authorization header");
    }
    authenticate(config, scheme, param).map_err(|_| anyhow::anyhow!("invalid credentials"))
}

/// Generate a JWT token for the given subject.
pub fn generate_token(config: &AuthConfig, subject: &str) -> Result<String> {
    if config.secret.is_empty() {
        anyhow::bail!("cannot issue token: auth secret is not configured");
    }
    let exp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs()
        + 86400; // 24 hours

    let claims = Claims {
        sub: subject.into(),
        exp,
    };

    let token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(config.secret.as_bytes()),
    )?;
    tracing::info!(subject, "auth token generated");
    Ok(token)
}

/// Validate a JWT token. Returns the subject if valid and still a configured
/// user, so removing a user from the config also revokes their tokens.
pub fn validate_token(config: &AuthConfig, token: &str) -> Result<String> {
    if config.secret.is_empty() {
        anyhow::bail!("cannot validate token: auth secret is not configured");
    }
    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(config.secret.as_bytes()),
        &Validation::default(),
    )
    .map_err(|e| {
        tracing::warn!(error = %e, "auth token validation failed");
        e
    })?;
    if !config.users.iter().any(|u| u.name == data.claims.sub) {
        anyhow::bail!("token subject is not a configured user");
    }
    Ok(data.claims.sub)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "test-secret-must-be-32-bytes-long";

    fn user(name: &str, password: &str) -> AuthUser {
        AuthUser {
            name: name.into(),
            password: password.into(),
        }
    }

    /// Enabled, with users `user1:pw1` and `alice:se:cret`.
    fn config() -> AuthConfig {
        AuthConfig {
            enabled: true,
            secret: SECRET.into(),
            users: vec![user("user1", "pw1"), user("alice", "se:cret")],
        }
    }

    fn basic(pair: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(pair)
    }

    #[test]
    fn token_roundtrip() {
        let config = config();
        let token = generate_token(&config, "user1").unwrap();
        let subject = validate_token(&config, &token).unwrap();
        assert_eq!(subject, "user1");
    }

    #[test]
    fn invalid_token() {
        assert!(validate_token(&config(), "garbage").is_err());
    }

    #[test]
    fn wrong_secret() {
        let config1 = config();
        let config2 = AuthConfig {
            secret: "secret2-must-be-at-least-32bytes!".into(),
            ..config()
        };
        let token = generate_token(&config1, "user1").unwrap();
        assert!(validate_token(&config2, &token).is_err());
    }

    #[test]
    fn token_of_a_removed_user_is_rejected() {
        let token = generate_token(&config(), "user1").unwrap();
        let without_user1 = AuthConfig {
            users: vec![user("alice", "se:cret")],
            ..config()
        };
        assert!(validate_token(&without_user1, &token).is_err());
    }

    #[test]
    fn default_is_disabled_with_empty_secret() {
        let config = AuthConfig::default();
        assert!(!config.enabled);
        assert!(config.secret.is_empty(), "must not ship a built-in secret");
        assert!(config.users.is_empty());
        // A disabled config is internally consistent.
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_rejects_enabled_without_secret() {
        let config = AuthConfig {
            secret: "   ".into(),
            ..config()
        };
        let e = config.validate().unwrap_err().to_string();
        assert!(e.contains("no secret"), "{e}");
    }

    #[test]
    fn validate_rejects_enabled_without_users() {
        let no_users = AuthConfig {
            users: vec![],
            ..config()
        };
        let e = no_users.validate().unwrap_err().to_string();
        assert!(e.contains("no user"), "{e}");
        assert!(config().validate().is_ok());
    }

    #[test]
    fn token_ops_refuse_empty_secret() {
        let config = AuthConfig {
            secret: String::new(),
            ..config()
        };
        assert!(generate_token(&config, "user1").is_err());
        assert!(validate_token(&config, "any.token.value").is_err());
    }

    #[test]
    fn user_parses_at_the_first_colon() {
        let u: AuthUser = "bob:pa:ss:word".parse().unwrap();
        assert_eq!(u, user("bob", "pa:ss:word"));
        for bad in ["bob", ":pw", "bob:", ""] {
            assert!(bad.parse::<AuthUser>().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn user_debug_hides_the_password() {
        let shown = format!("{:?}", user("bob", "hunter2"));
        assert!(shown.contains("bob"));
        assert!(!shown.contains("hunter2"), "{shown}");
    }

    #[test]
    fn verify_credentials_needs_matching_name_and_password() {
        let config = config();
        assert!(verify_credentials(&config, "user1", "pw1"));
        assert!(verify_credentials(&config, "alice", "se:cret"));
        assert!(!verify_credentials(&config, "user1", "se:cret"));
        assert!(!verify_credentials(&config, "user1", "pw"));
        assert!(!verify_credentials(&config, "nobody", "pw1"));
        assert!(!verify_credentials(&AuthConfig::default(), "", ""));
    }

    #[test]
    fn authenticate_schemes() {
        let config = config();
        assert_eq!(
            authenticate(&config, "Basic", &basic("alice:se:cret")),
            Ok("alice".into())
        );
        assert_eq!(
            authenticate(&config, "plain", "user1:pw1"),
            Ok("user1".into())
        );
        let token = generate_token(&config, "user1").unwrap();
        assert_eq!(authenticate(&config, "BEARER", &token), Ok("user1".into()));
        assert_eq!(
            authenticate(&config, "Digest", "x"),
            Err(AuthFailure::UnsupportedScheme)
        );
    }

    #[test]
    fn authenticate_failures_are_indistinguishable() {
        let config = config();
        for (scheme, param) in [
            ("Basic", basic("user1:wrong")),
            ("Basic", basic("nobody:pw1")),
            ("Basic", basic("no-colon")),
            ("Basic", "!!not base64!!".to_string()),
            ("Plain", "user1:wrong".to_string()),
            ("Plain", "nobody:pw1".to_string()),
            ("Bearer", "not.a.jwt".to_string()),
        ] {
            assert_eq!(
                authenticate(&config, scheme, &param),
                Err(AuthFailure::Unauthorized),
                "{scheme} {param}"
            );
        }
    }

    #[test]
    fn authorization_header_allows_all_when_auth_disabled() {
        let config = AuthConfig::default();
        assert!(validate_authorization(&config, None).is_ok());
        assert!(validate_authorization(&config, Some("garbage")).is_ok());
    }

    #[test]
    fn authorization_header_accepts_bearer_and_basic() {
        let config = config();
        let token = generate_token(&config, "alice").unwrap();
        let header = format!("Bearer {token}");
        assert_eq!(
            validate_authorization(&config, Some(&header)).unwrap(),
            "alice"
        );
        let header = format!("Basic {}", basic("user1:pw1"));
        assert_eq!(
            validate_authorization(&config, Some(&header)).unwrap(),
            "user1"
        );
    }

    #[test]
    fn authorization_header_rejects_missing_malformed_and_wrong() {
        let config = config();
        for header in [
            None,
            Some("Bearer"),
            Some("Bearer not-a-jwt"),
            Some("Plain user1:pw1"),
            Some(&*format!("Basic {}", basic("user1:wrong"))),
        ] {
            assert!(
                validate_authorization(&config, header).is_err(),
                "{header:?}"
            );
        }
    }
}
