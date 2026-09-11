//! 有界密码计算与内存登录表；登录失效仅取消传输，不取消已接纳的 Runtime Run。

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use argon2::{
    Algorithm, Argon2, Params, PasswordHash, PasswordHasher, PasswordVerifier, Version,
    password_hash::SaltString,
};
use assistant_protocol::{ClientCompatibility, SecretValue};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use tokio::{sync::Semaphore, time::Instant};
use tokio_util::sync::CancellationToken;

use super::AccessError;

const LOGIN_LIFETIME: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const MAX_SESSIONS: usize = 64;

#[derive(Clone)]
pub(crate) struct LoginSession {
    id: [u8; 32],
    pub(crate) expires_at_ms: u64,
    compatibility: ClientCompatibility,
    deadline: Instant,
    cancelled: CancellationToken,
}

impl LoginSession {
    pub(crate) fn valid(&self) -> bool {
        !self.cancelled.is_cancelled() && Instant::now() < self.deadline
    }

    pub(crate) async fn ended(&self) {
        tokio::select! {
            () = self.cancelled.cancelled() => {},
            () = tokio::time::sleep_until(self.deadline) => {},
        }
    }
}

#[derive(Clone)]
pub(crate) struct AccessPermit {
    pub(crate) native: bool,
    session: Option<LoginSession>,
    listener: CancellationToken,
}

impl AccessPermit {
    pub(crate) fn compatibility(&self) -> Option<&ClientCompatibility> {
        self.session.as_ref().map(|session| &session.compatibility)
    }

    /// 登录身份仅用于同一登录的连接限额，不暴露 token。None 是本机原生凭据域。
    pub(crate) fn login_id(&self) -> Option<[u8; 32]> {
        self.session.as_ref().map(|session| session.id)
    }

    pub(crate) fn expires_at_ms(&self) -> u64 {
        self.session
            .as_ref()
            .map_or(0, |session| session.expires_at_ms)
    }

    pub(crate) fn new(
        native: bool,
        session: Option<LoginSession>,
        listener: CancellationToken,
    ) -> Self {
        Self {
            native,
            session,
            listener,
        }
    }

    pub(crate) fn check(&self) -> Result<(), AccessError> {
        if self.listener.is_cancelled()
            || self
                .session
                .as_ref()
                .is_some_and(|session| !session.valid())
        {
            return Err(AccessError::Unauthorized);
        }
        Ok(())
    }

    pub(crate) async fn ended(&self) {
        let session_end = async {
            match &self.session {
                Some(session) => session.ended().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            () = self.listener.cancelled() => {},
            () = session_end => {},
        }
    }

    pub(crate) fn logout(&self) {
        if let Some(session) = &self.session {
            session.cancelled.cancel();
        }
    }
}

struct CredentialState {
    password_hash: Option<String>,
    sessions: HashMap<[u8; 32], LoginSession>,
    attempts: f64,
    refilled: Instant,
}

pub(crate) struct Credentials {
    state: Mutex<CredentialState>,
    hashing: Arc<Semaphore>,
}

impl Credentials {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(CredentialState {
                password_hash: None,
                sessions: HashMap::new(),
                attempts: 5.0,
                refilled: Instant::now(),
            }),
            hashing: Arc::new(Semaphore::new(2)),
        }
    }

    pub(crate) fn replace_password(&self, password_hash: Option<String>) {
        let mut state = self.state.lock().expect("credential state poisoned");
        if state.password_hash != password_hash {
            for session in state.sessions.values() {
                session.cancelled.cancel();
            }
            state.sessions.clear();
            state.password_hash = password_hash;
        }
    }

    pub(crate) async fn hash_password(&self, password: SecretValue) -> Result<String, AccessError> {
        validate_password(password.expose())?;
        let slot = self
            .hashing
            .clone()
            .try_acquire_owned()
            .map_err(|_| AccessError::Busy)?;
        tokio::task::spawn_blocking(move || {
            let _slot = slot;
            let mut bytes = [0_u8; 16];
            getrandom::fill(&mut bytes).map_err(|_| AccessError::Unavailable)?;
            let salt = SaltString::encode_b64(&bytes).map_err(|_| AccessError::Unavailable)?;
            password_hasher()
                .hash_password(password.expose().as_bytes(), &salt)
                .map(|hash| hash.to_string())
                .map_err(|_| AccessError::Unavailable)
        })
        .await
        .map_err(|_| AccessError::Unavailable)?
    }

    pub(crate) async fn login(
        &self,
        password: SecretValue,
        compatibility: ClientCompatibility,
    ) -> Result<(SecretValue, LoginSession), AccessError> {
        let expected = {
            let mut state = self.state.lock().expect("credential state poisoned");
            let now = Instant::now();
            state.attempts =
                (state.attempts + now.duration_since(state.refilled).as_secs_f64() * 0.5).min(5.0);
            state.refilled = now;
            if state.attempts < 1.0 {
                return Err(AccessError::Busy);
            }
            state.attempts -= 1.0;
            state
                .password_hash
                .clone()
                .ok_or(AccessError::Unauthorized)?
        };
        validate_password(password.expose()).map_err(|_| AccessError::Unauthorized)?;
        let slot = self
            .hashing
            .clone()
            .try_acquire_owned()
            .map_err(|_| AccessError::Busy)?;
        let verify_hash = expected.clone();
        let valid = tokio::task::spawn_blocking(move || {
            let _slot = slot;
            PasswordHash::new(&verify_hash).is_ok_and(|hash| {
                password_hasher()
                    .verify_password(password.expose().as_bytes(), &hash)
                    .is_ok()
            })
        })
        .await
        .map_err(|_| AccessError::Unavailable)?;
        let mut state = self.state.lock().expect("credential state poisoned");
        // 散列完成后再次核对，禁止旧密码计算结果跨过改密提交边界。
        if !valid || state.password_hash.as_ref() != Some(&expected) {
            return Err(AccessError::Unauthorized);
        }
        issue(&mut state, compatibility)
    }

    pub(crate) fn issue(
        &self,
        permit: &AccessPermit,
        compatibility: ClientCompatibility,
    ) -> Result<(SecretValue, LoginSession), AccessError> {
        let mut state = self.state.lock().expect("credential state poisoned");
        permit.check()?;
        issue(&mut state, compatibility)
    }

    /// 快捷登录交换为当前页面版本的独立会话，不改原 token 的不可变声明。
    pub(crate) fn exchange(
        &self,
        token: &str,
        compatibility: ClientCompatibility,
    ) -> Result<(SecretValue, LoginSession), AccessError> {
        let mut state = self.state.lock().expect("credential state poisoned");
        if token.len() != 43
            || !state
                .sessions
                .get(&digest(token))
                .is_some_and(LoginSession::valid)
        {
            return Err(AccessError::Unauthorized);
        }
        issue(&mut state, compatibility)
    }

    pub(crate) fn authenticate(&self, token: &str) -> Option<LoginSession> {
        if token.len() != 43 {
            return None;
        }
        let state = self.state.lock().expect("credential state poisoned");
        state
            .sessions
            .get(&digest(token))
            .filter(|session| session.valid())
            .cloned()
    }
}

fn issue(
    state: &mut CredentialState,
    compatibility: ClientCompatibility,
) -> Result<(SecretValue, LoginSession), AccessError> {
    if !compatibility.is_valid() {
        return Err(AccessError::Invalid("软件版本声明无效。"));
    }
    state.sessions.retain(|_, session| session.valid());
    if state.sessions.len() >= MAX_SESSIONS {
        return Err(AccessError::Busy);
    }
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| AccessError::Unavailable)?;
    let token = URL_SAFE_NO_PAD.encode(bytes);
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AccessError::Unavailable)?
        .as_millis();
    let session = LoginSession {
        id: digest(&token),
        compatibility,
        expires_at_ms: (now_ms + LOGIN_LIFETIME.as_millis()) as u64,
        deadline: Instant::now() + LOGIN_LIFETIME,
        cancelled: CancellationToken::new(),
    };
    state.sessions.insert(digest(&token), session.clone());
    Ok((SecretValue::new(token), session))
}

fn digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

fn password_hasher() -> Argon2<'static> {
    Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        Params::new(19 * 1024, 2, 1, Some(32)).expect("fixed parameters"),
    )
}

fn validate_password(password: &str) -> Result<(), AccessError> {
    if password.trim().is_empty() || password.len() > 1024 {
        return Err(AccessError::Invalid(
            "密码需为 1—1024 字节，且不能全部为空白。",
        ));
    }
    Ok(())
}

pub(super) fn validate_hash(hash: &str) -> Result<(), AccessError> {
    let hash = PasswordHash::new(hash).map_err(|_| AccessError::Invalid("密码配置无效。"))?;
    if hash.algorithm.as_str() != "argon2id"
        || hash.version != Some(19)
        || hash.params.get_decimal("m") != Some(19 * 1024)
        || hash.params.get_decimal("t") != Some(2)
        || hash.params.get_decimal("p") != Some(1)
        || hash.hash.as_ref().map(|value| value.len()) != Some(32)
        || hash.salt.is_none()
    {
        return Err(AccessError::Invalid("密码配置无效。"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn password_change_revokes_sessions_and_does_not_accept_old_password() {
        let credentials = Credentials::new();
        let first = credentials
            .hash_password(SecretValue::new("first password".into()))
            .await
            .unwrap();
        validate_hash(&first).unwrap();
        credentials.replace_password(Some(first));
        let (token, session) = credentials
            .login(
                SecretValue::new("first password".into()),
                ClientCompatibility::current(),
            )
            .await
            .unwrap();
        assert!(credentials.authenticate(token.expose()).is_some());
        let second = credentials
            .hash_password(SecretValue::new("second password".into()))
            .await
            .unwrap();
        credentials.replace_password(Some(second));
        assert!(!session.valid());
        assert!(credentials.authenticate(token.expose()).is_none());
        assert!(
            credentials
                .login(
                    SecretValue::new("first password".into()),
                    ClientCompatibility::current()
                )
                .await
                .is_err()
        );
        assert!(
            credentials
                .login(
                    SecretValue::new("second password".into()),
                    ClientCompatibility::current()
                )
                .await
                .is_ok()
        );
    }

    #[test]
    fn native_and_ordinary_login_lifetimes_are_separate_and_bounded() {
        let credentials = Credentials::new();
        let native = AccessPermit::new(true, None, CancellationToken::new());
        let (token, session) = credentials
            .issue(&native, ClientCompatibility::current())
            .unwrap();
        let ordinary = AccessPermit::new(false, Some(session), CancellationToken::new());
        let (browser_token, _) = credentials
            .issue(&ordinary, ClientCompatibility::current())
            .unwrap();
        ordinary.logout();
        assert!(credentials.authenticate(token.expose()).is_none());
        assert!(credentials.authenticate(browser_token.expose()).is_some());
        assert!(native.check().is_ok());
        for _ in 1..MAX_SESSIONS {
            credentials
                .issue(&native, ClientCompatibility::current())
                .unwrap();
        }
        assert!(matches!(
            credentials.issue(&native, ClientCompatibility::current()),
            Err(AccessError::Busy)
        ));
    }

    #[tokio::test]
    async fn login_attempts_and_expired_sessions_are_bounded() {
        let credentials = Credentials::new();
        for _ in 0..5 {
            assert!(matches!(
                credentials
                    .login(
                        SecretValue::new("test".into()),
                        ClientCompatibility::current()
                    )
                    .await,
                Err(AccessError::Unauthorized)
            ));
        }
        assert!(matches!(
            credentials
                .login(
                    SecretValue::new("test".into()),
                    ClientCompatibility::current()
                )
                .await,
            Err(AccessError::Busy)
        ));
        let native = AccessPermit::new(true, None, CancellationToken::new());
        let (token, _) = credentials
            .issue(&native, ClientCompatibility::current())
            .unwrap();
        credentials
            .state
            .lock()
            .unwrap()
            .sessions
            .get_mut(&digest(token.expose()))
            .unwrap()
            .deadline = Instant::now();
        assert!(credentials.authenticate(token.expose()).is_none());
        assert!(validate_password("   ").is_err());
        assert!(validate_password(&"x".repeat(1025)).is_err());
    }
}
