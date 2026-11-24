//! Signing in to the panel: one administrator. The first
//! visit sets the password, with a setup code that was printed in the server's
//! log, so that whoever finds the page first is not automatically the
//! administrator. Sessions are random tokens in an HttpOnly cookie; only their
//! hashes are stored.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use subtle::ConstantTimeEq;

pub const COOKIE: &str = "nk_session";
const SESSION_DAYS: i64 = 7;
pub const MIN_PASSWORD_CHARS: usize = 10;
const MAX_PASSWORD_BYTES: usize = 1024;
/// More wrong attempts than this within the window are refused for the rest of it.
const MAX_FAILURES: usize = 5;
const WINDOW: Duration = Duration::from_secs(60);

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS admin (
    id            INTEGER PRIMARY KEY CHECK (id = 1),
    password_hash TEXT    NOT NULL,
    created_ms    INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS web_sessions (
    token_hash  TEXT PRIMARY KEY,
    created_ms  INTEGER NOT NULL,
    expires_ms  INTEGER NOT NULL
);
"#;

#[derive(Debug, PartialEq, Eq)]
pub enum AuthError {
    AlreadyInitialized,
    BadSetupCode,
    WeakPassword,
    BadCredentials,
    /// Seconds until another attempt is allowed.
    TooManyAttempts(u64),
    Internal(String),
}

impl From<sqlx::Error> for AuthError {
    fn from(e: sqlx::Error) -> Self {
        AuthError::Internal(e.to_string())
    }
}

pub struct Auth {
    pool: SqlitePool,
    setup_code: Mutex<Option<String>>,
    failures: Mutex<Vec<Instant>>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn token_hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Letters and digits that cannot be mistaken for each other, in groups of four.
fn random_setup_code() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut bytes = [0u8; 8];
    OsRng.fill_bytes(&mut bytes);
    let chars: String = bytes
        .iter()
        .map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char)
        .collect();
    format!("{}-{}", &chars[..4], &chars[4..])
}

impl Auth {
    pub async fn new(pool: SqlitePool) -> Result<Self, sqlx::Error> {
        sqlx::raw_sql(SCHEMA).execute(&pool).await?;
        Ok(Self {
            pool,
            setup_code: Mutex::new(None),
            failures: Mutex::new(Vec::new()),
        })
    }

    pub async fn initialized(&self) -> Result<bool, sqlx::Error> {
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM admin")
            .fetch_one(&self.pool)
            .await?;
        Ok(n > 0)
    }

    /// Makes the setup code, if the administrator is not set up yet. It lives in
    /// memory only: a restart before setup makes a new one.
    pub async fn prepare_setup(&self) -> Result<Option<String>, sqlx::Error> {
        if self.initialized().await? {
            return Ok(None);
        }
        let code = random_setup_code();
        *self.setup_code.lock().unwrap() = Some(code.clone());
        Ok(Some(code))
    }

    fn blocked(&self) -> Option<u64> {
        let mut failures = self.failures.lock().unwrap();
        failures.retain(|t| t.elapsed() < WINDOW);
        if failures.len() >= MAX_FAILURES {
            let oldest = failures[0];
            return Some(WINDOW.saturating_sub(oldest.elapsed()).as_secs().max(1));
        }
        None
    }

    fn fail(&self) {
        self.failures.lock().unwrap().push(Instant::now());
    }

    async fn new_session(&self) -> Result<String, AuthError> {
        let token = random_token();
        let now = now_ms();
        sqlx::query("DELETE FROM web_sessions WHERE expires_ms < ?")
            .bind(now)
            .execute(&self.pool)
            .await?;
        sqlx::query("INSERT INTO web_sessions (token_hash, created_ms, expires_ms) VALUES (?, ?, ?)")
            .bind(token_hash(&token))
            .bind(now)
            .bind(now + SESSION_DAYS * 24 * 3600 * 1000)
            .execute(&self.pool)
            .await?;
        Ok(token)
    }

    /// Sets the password the first time; returns the session token.
    pub async fn setup(&self, password: &str, code: &str) -> Result<String, AuthError> {
        if self.initialized().await? {
            return Err(AuthError::AlreadyInitialized);
        }
        if let Some(wait) = self.blocked() {
            return Err(AuthError::TooManyAttempts(wait));
        }
        let expected = self.setup_code.lock().unwrap().clone();
        let given = code.trim().to_ascii_uppercase();
        let ok = expected
            .as_deref()
            .is_some_and(|e| bool::from(e.as_bytes().ct_eq(given.as_bytes())));
        if !ok {
            self.fail();
            return Err(AuthError::BadSetupCode);
        }
        if password.chars().count() < MIN_PASSWORD_CHARS || password.len() > MAX_PASSWORD_BYTES {
            return Err(AuthError::WeakPassword);
        }
        let hash = hash_password(password).await?;
        let inserted = sqlx::query("INSERT OR IGNORE INTO admin (id, password_hash, created_ms) VALUES (1, ?, ?)")
            .bind(hash)
            .bind(now_ms())
            .execute(&self.pool)
            .await?
            .rows_affected();
        if inserted == 0 {
            return Err(AuthError::AlreadyInitialized);
        }
        *self.setup_code.lock().unwrap() = None;
        self.new_session().await
    }

    pub async fn login(&self, password: &str) -> Result<String, AuthError> {
        if let Some(wait) = self.blocked() {
            return Err(AuthError::TooManyAttempts(wait));
        }
        let stored: Option<String> = sqlx::query_scalar("SELECT password_hash FROM admin WHERE id = 1")
            .fetch_optional(&self.pool)
            .await?;
        let ok = match stored {
            Some(hash) if password.len() <= MAX_PASSWORD_BYTES => verify_password(password, hash).await,
            _ => false,
        };
        if !ok {
            self.fail();
            return Err(AuthError::BadCredentials);
        }
        self.new_session().await
    }

    pub async fn valid(&self, token: &str) -> bool {
        let found: Result<Option<i64>, _> =
            sqlx::query_scalar("SELECT expires_ms FROM web_sessions WHERE token_hash = ?")
                .bind(token_hash(token))
                .fetch_optional(&self.pool)
                .await;
        matches!(found, Ok(Some(expires)) if expires > now_ms())
    }

    pub async fn logout(&self, token: &str) {
        let _ = sqlx::query("DELETE FROM web_sessions WHERE token_hash = ?")
            .bind(token_hash(token))
            .execute(&self.pool)
            .await;
    }
}

async fn hash_password(password: &str) -> Result<String, AuthError> {
    let password = password.to_string();
    tokio::task::spawn_blocking(move || {
        Argon2::default()
            .hash_password(password.as_bytes())
            .map(|h| h.to_string())
            .map_err(|e| AuthError::Internal(e.to_string()))
    })
    .await
    .map_err(|e| AuthError::Internal(e.to_string()))?
}

async fn verify_password(password: &str, hash: String) -> bool {
    let password = password.to_string();
    tokio::task::spawn_blocking(move || {
        PasswordHash::new(&hash)
            .is_ok_and(|parsed| Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok())
    })
    .await
    .unwrap_or(false)
}

/// The session token in a `Cookie` header, if there is one.
pub fn token_from_cookies(header: &str) -> Option<&str> {
    header.split(';').find_map(|part| {
        let (name, value) = part.trim().split_once('=')?;
        (name == COOKIE).then_some(value)
    })
}

pub fn session_cookie(token: &str, secure: bool) -> String {
    format!(
        "{COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}{}",
        SESSION_DAYS * 24 * 3600,
        if secure { "; Secure" } else { "" }
    )
}

pub fn clear_cookie(secure: bool) -> String {
    format!(
        "{COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0{}",
        if secure { "; Secure" } else { "" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn auth() -> Auth {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        Auth::new(pool).await.unwrap()
    }

    #[tokio::test]
    async fn first_run_setup_then_login() {
        let auth = auth().await;
        assert!(!auth.initialized().await.unwrap());
        let code = auth.prepare_setup().await.unwrap().unwrap();
        assert_eq!(code.len(), 9);

        assert_eq!(
            auth.setup("a long enough password", "WRONG-CODE").await,
            Err(AuthError::BadSetupCode)
        );
        assert_eq!(auth.setup("short", &code).await, Err(AuthError::WeakPassword));
        let token = auth
            .setup("a long enough password", &code.to_lowercase())
            .await
            .unwrap();
        assert!(auth.valid(&token).await);
        assert!(auth.initialized().await.unwrap());

        // Setup works once.
        assert_eq!(
            auth.setup("another long password", &code).await,
            Err(AuthError::AlreadyInitialized)
        );
        assert!(auth.prepare_setup().await.unwrap().is_none());

        assert_eq!(auth.login("not the password").await, Err(AuthError::BadCredentials));
        let again = auth.login("a long enough password").await.unwrap();
        assert_ne!(again, token);
        assert!(auth.valid(&again).await);

        auth.logout(&again).await;
        assert!(!auth.valid(&again).await);
        assert!(auth.valid(&token).await, "other sessions stay");
        assert!(!auth.valid("made-up").await);
    }

    #[tokio::test]
    async fn too_many_wrong_attempts_are_refused_for_a_while() {
        let auth = auth().await;
        let code = auth.prepare_setup().await.unwrap().unwrap();
        auth.setup("a long enough password", &code).await.unwrap();
        for _ in 0..MAX_FAILURES {
            assert_eq!(auth.login("wrong").await, Err(AuthError::BadCredentials));
        }
        match auth.login("a long enough password").await {
            Err(AuthError::TooManyAttempts(wait)) => assert!((1..=60).contains(&wait)),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn without_a_setup_code_nobody_can_set_up() {
        let auth = auth().await; // prepare_setup never called
        assert_eq!(
            auth.setup("a long enough password", "ANYTHING").await,
            Err(AuthError::BadSetupCode)
        );
    }

    #[test]
    fn reads_the_session_cookie() {
        assert_eq!(token_from_cookies("a=1; nk_session=abc; b=2"), Some("abc"));
        assert_eq!(token_from_cookies("a=1"), None);
        assert!(session_cookie("t", true).contains("Secure"));
        assert!(session_cookie("t", false).contains("HttpOnly; SameSite=Strict"));
    }
}
