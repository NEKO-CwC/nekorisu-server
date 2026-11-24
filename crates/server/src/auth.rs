//! Signing in to the panel: one administrator. The first
//! visit sets the password, with a setup code that was printed in the server's
//! log, so that whoever finds the page first is not automatically the
//! administrator. Sessions are random tokens in an HttpOnly cookie; only their
//! hashes are stored.
//!
//! Wrong passwords are limited per client address (`crate::limit`): an
//! attempt takes a place in the limiter before the password is checked and
//! gives it back only when it was right, so attempts running at the same
//! time cannot all get in before the first failure is counted. A ceiling
//! across all clients is the backstop. At most a couple of argon2 checks run
//! at once, whoever asks. Login (including the OAuth consent page) and setup
//! are counted apart.

use std::sync::Mutex;
use std::time::Duration;

use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use subtle::ConstantTimeEq;

use crate::limit::{Limiter, RateKey, Rule};

pub const COOKIE: &str = "nk_session";
const SESSION_DAYS: i64 = 7;
pub const MIN_PASSWORD_CHARS: usize = 10;
const MAX_PASSWORD_BYTES: usize = 1024;
/// More wrong attempts than this from one client (an IPv4 address or an
/// IPv6 /64) within the window are refused for the rest of it.
const MAX_FAILURES: usize = 5;
const WINDOW: Duration = Duration::from_secs(60);
/// Wrong attempts from all clients together within the window: ten times one
/// client's allowance. Someone mistyping on a few devices stays far below it,
/// and it takes more than ten addresses failing at the same time before
/// everyone waits out the minute (a signed-in panel keeps working). It caps a
/// guesser with many addresses at 50 tries a minute, each one an argon2 hash
/// against a password of at least 10 characters.
const MAX_FAILURES_ALL: usize = 10 * MAX_FAILURES;
/// Clients tracked at once per limiter. Above the ceiling, so it cannot fill
/// with live clients (see `crate::limit`); it bounds the memory taken by
/// expired ones between sweeps (each is well under 200 bytes).
const MAX_CLIENTS: usize = 1024;
const RULE: Rule = Rule {
    per_key: MAX_FAILURES,
    global: MAX_FAILURES_ALL,
    window: WINDOW,
    max_keys: MAX_CLIENTS,
};
/// argon2 checks running at once (each takes about 19 MiB).
const MAX_HASHING: usize = 2;

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
    login_attempts: Limiter,
    setup_attempts: Limiter,
    hashing: tokio::sync::Semaphore,
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
            login_attempts: Limiter::new("login", RULE),
            setup_attempts: Limiter::new("setup code", RULE),
            hashing: tokio::sync::Semaphore::new(MAX_HASHING),
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

    /// Sets the password the first time; returns the session token. Wrong
    /// setup codes are counted against `client`.
    pub async fn setup(&self, password: &str, code: &str, client: RateKey) -> Result<String, AuthError> {
        if self.initialized().await? {
            return Err(AuthError::AlreadyInitialized);
        }
        let attempt = self
            .setup_attempts
            .reserve(client)
            .map_err(AuthError::TooManyAttempts)?;
        let expected = self.setup_code.lock().unwrap().clone();
        let given = code.trim().to_ascii_uppercase();
        let ok = expected
            .as_deref()
            .is_some_and(|e| bool::from(e.as_bytes().ct_eq(given.as_bytes())));
        if !ok {
            return Err(AuthError::BadSetupCode);
        }
        attempt.give_back();
        if password.chars().count() < MIN_PASSWORD_CHARS || password.len() > MAX_PASSWORD_BYTES {
            return Err(AuthError::WeakPassword);
        }
        let hash = {
            let _permit = self
                .hashing
                .acquire()
                .await
                .map_err(|e| AuthError::Internal(e.to_string()))?;
            hash_password(password).await?
        };
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

    /// Checks the administrator password, through the login limiter; a wrong
    /// one is counted against `client`. Used by the panel login and by the
    /// OAuth consent page.
    pub async fn check_password(&self, password: &str, client: RateKey) -> Result<(), AuthError> {
        let attempt = self
            .login_attempts
            .reserve(client)
            .map_err(AuthError::TooManyAttempts)?;
        let stored: Option<String> = match sqlx::query_scalar("SELECT password_hash FROM admin WHERE id = 1")
            .fetch_optional(&self.pool)
            .await
        {
            Ok(stored) => stored,
            Err(e) => {
                attempt.give_back(); // the server failed, not the person
                return Err(e.into());
            }
        };
        let ok = match stored {
            Some(hash) if password.len() <= MAX_PASSWORD_BYTES => {
                let _permit = self
                    .hashing
                    .acquire()
                    .await
                    .map_err(|e| AuthError::Internal(e.to_string()))?;
                verify_password(password, hash).await
            }
            _ => false,
        };
        if !ok {
            return Err(AuthError::BadCredentials);
        }
        attempt.give_back();
        Ok(())
    }

    pub async fn login(&self, password: &str, client: RateKey) -> Result<String, AuthError> {
        self.check_password(password, client).await?;
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

    /// One client, and another.
    const A: RateKey = RateKey::V4(0xcb00_7101); // 203.0.113.1
    const B: RateKey = RateKey::V4(0xcb00_7102); // 203.0.113.2

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
            auth.setup("a long enough password", "WRONG-CODE", A).await,
            Err(AuthError::BadSetupCode)
        );
        assert_eq!(auth.setup("short", &code, A).await, Err(AuthError::WeakPassword));
        let token = auth
            .setup("a long enough password", &code.to_lowercase(), A)
            .await
            .unwrap();
        assert!(auth.valid(&token).await);
        assert!(auth.initialized().await.unwrap());

        // Setup works once.
        assert_eq!(
            auth.setup("another long password", &code, A).await,
            Err(AuthError::AlreadyInitialized)
        );
        assert!(auth.prepare_setup().await.unwrap().is_none());

        assert_eq!(auth.login("not the password", A).await, Err(AuthError::BadCredentials));
        let again = auth.login("a long enough password", A).await.unwrap();
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
        auth.setup("a long enough password", &code, A).await.unwrap();
        for _ in 0..MAX_FAILURES {
            assert_eq!(auth.login("wrong", A).await, Err(AuthError::BadCredentials));
        }
        match auth.login("a long enough password", A).await {
            Err(AuthError::TooManyAttempts(wait)) => assert!((1..=60).contains(&wait)),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn concurrent_wrong_passwords_cannot_get_past_the_limit() {
        let auth = std::sync::Arc::new(auth().await);
        let code = auth.prepare_setup().await.unwrap().unwrap();
        auth.setup("a long enough password", &code, A).await.unwrap();
        let tries: Vec<_> = (0..40)
            .map(|_| {
                let auth = auth.clone();
                tokio::spawn(async move { auth.login("wrong", A).await })
            })
            .collect();
        let mut wrong = 0;
        let mut refused = 0;
        for t in tries {
            match t.await.unwrap() {
                Err(AuthError::BadCredentials) => wrong += 1,
                Err(AuthError::TooManyAttempts(_)) => refused += 1,
                other => panic!("{other:?}"),
            }
        }
        assert_eq!((wrong, refused), (MAX_FAILURES, 40 - MAX_FAILURES));
        assert!(matches!(
            auth.check_password("a long enough password", A).await,
            Err(AuthError::TooManyAttempts(_))
        ));
    }

    #[tokio::test]
    async fn a_right_password_gives_its_place_back() {
        let auth = auth().await;
        let code = auth.prepare_setup().await.unwrap().unwrap();
        auth.setup("a long enough password", &code, A).await.unwrap();
        for _ in 0..MAX_FAILURES - 1 {
            assert_eq!(auth.login("wrong", A).await, Err(AuthError::BadCredentials));
        }
        for _ in 0..3 {
            assert!(auth.check_password("a long enough password", A).await.is_ok());
        }
        assert_eq!(auth.login("wrong", A).await, Err(AuthError::BadCredentials));
        assert!(matches!(
            auth.login("a long enough password", A).await,
            Err(AuthError::TooManyAttempts(_))
        ));
    }

    #[tokio::test]
    async fn setup_and_login_are_counted_apart() {
        let auth = auth().await;
        let code = auth.prepare_setup().await.unwrap().unwrap();
        for _ in 0..MAX_FAILURES {
            assert_eq!(
                auth.setup("a long enough password", "WRONG-CODE", A).await,
                Err(AuthError::BadSetupCode)
            );
        }
        assert!(matches!(
            auth.setup("a long enough password", &code, A).await,
            Err(AuthError::TooManyAttempts(_))
        ));
        // Wrong setup codes do not lock the login (nor the consent page)…
        assert_eq!(auth.login("anything", A).await, Err(AuthError::BadCredentials));
        // …nor another client's setup.
        assert!(auth.setup("a long enough password", &code, B).await.is_ok());
    }

    #[tokio::test]
    async fn one_client_locked_out_does_not_lock_out_another() {
        let auth = auth().await;
        let code = auth.prepare_setup().await.unwrap().unwrap();
        auth.setup("a long enough password", &code, A).await.unwrap();
        for _ in 0..MAX_FAILURES {
            assert_eq!(auth.login("wrong", A).await, Err(AuthError::BadCredentials));
        }
        assert!(matches!(
            auth.login("a long enough password", A).await,
            Err(AuthError::TooManyAttempts(_))
        ));
        assert!(auth.login("a long enough password", B).await.is_ok());
    }

    #[tokio::test]
    async fn many_clients_together_reach_the_ceiling() {
        // No administrator yet: wrong passwords are refused without hashing, and still count.
        let auth = auth().await;
        for i in 0..MAX_FAILURES_ALL as u32 {
            let client = RateKey::V4(0xc633_6400 + i / MAX_FAILURES as u32); // 198.51.100.x
            assert_eq!(
                auth.check_password("wrong", client).await,
                Err(AuthError::BadCredentials)
            );
        }
        assert!(matches!(
            auth.check_password("wrong", B).await,
            Err(AuthError::TooManyAttempts(_))
        ));
        // Setup codes are counted apart.
        assert_eq!(
            auth.setup("a long enough password", "WRONG-CODE", B).await,
            Err(AuthError::BadSetupCode)
        );
    }

    #[tokio::test]
    async fn without_a_setup_code_nobody_can_set_up() {
        let auth = auth().await; // prepare_setup never called
        assert_eq!(
            auth.setup("a long enough password", "ANYTHING", A).await,
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
