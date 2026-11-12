use std::{
    collections::HashMap,
    io::Read,
    sync::Mutex,
    time::{Duration, Instant},
};

use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use hmac::{Hmac, Mac};
use serde::{Deserialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

use crate::record_protocol::{OAuthPoll, OAuthPollRequest, OAuthProvider, OAuthStart, SignedIn};

use super::{ApiError, Result, internal, random_hex};

const SIGN_IN_TTL: Duration = Duration::from_secs(5 * 60);
const MAX_PENDING_SIGN_INS: usize = 128;
const MAX_PROVIDER_BODY: u64 = 64 * 1024;
const POLL_DOMAIN: &[u8] = b"surfd-native/oauth-poll/v1";

#[derive(Default)]
pub(super) struct OAuth {
    providers: Vec<ProviderConfig>,
    pending: Mutex<HashMap<String, PendingSignIn>>,
}

struct ProviderConfig {
    provider: OAuthProvider,
    client_id: String,
    client_secret: String,
    redirect_uri: String,
}

struct PendingSignIn {
    provider: OAuthProvider,
    poll_digest: [u8; 32],
    expires: Instant,
    status: SignInStatus,
}

enum SignInStatus {
    Waiting(String),
    Exchanging,
    Ready(ProviderIdentity),
    Failed,
}

pub(super) struct ProviderIdentity {
    pub(super) provider: OAuthProvider,
    pub(super) subject: String,
    pub(super) name: String,
}

impl OAuth {
    pub(super) fn from_env() -> std::result::Result<Self, String> {
        let origin = optional_env("SURF_OAUTH_PUBLIC_URL")?;
        let mut providers = Vec::new();
        for (provider, id_env, secret_env) in [
            (
                OAuthProvider::Google,
                "SURF_GOOGLE_CLIENT_ID",
                "SURF_GOOGLE_CLIENT_SECRET",
            ),
            (OAuthProvider::X, "SURF_X_CLIENT_ID", "SURF_X_CLIENT_SECRET"),
        ] {
            match (optional_env(id_env)?, optional_env(secret_env)?) {
                (None, None) => {}
                (Some(client_id), Some(client_secret)) => {
                    let origin = origin
                        .as_deref()
                        .ok_or("Set SURF_OAUTH_PUBLIC_URL to the HTTPS records service origin.")?;
                    providers.push(ProviderConfig::new(
                        provider,
                        client_id,
                        client_secret,
                        origin,
                    )?);
                }
                _ => return Err(format!("Set both {id_env} and {secret_env}.")),
            }
        }
        Ok(Self {
            providers,
            pending: Mutex::default(),
        })
    }

    pub(super) fn providers(&self) -> Vec<OAuthProvider> {
        self.providers
            .iter()
            .map(|config| config.provider)
            .collect()
    }

    pub(super) fn start(&self, provider: OAuthProvider) -> Result<OAuthStart> {
        let config = self.provider(provider)?;
        let state = random_hex::<32>()?;
        let token = random_hex::<32>()?;
        let verifier = random_hex::<32>()?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let scope = match provider {
            OAuthProvider::Google => "openid profile",
            OAuthProvider::X => "tweet.read users.read",
        };
        let url = format!(
            "{}?{}",
            provider.authorization_endpoint(),
            form(&[
                ("client_id", &config.client_id),
                ("redirect_uri", &config.redirect_uri),
                ("response_type", "code"),
                ("scope", scope),
                ("state", &state),
                ("code_challenge", &challenge),
                ("code_challenge_method", "S256"),
            ])
        );
        let now = Instant::now();
        let mut pending = self.pending.lock().map_err(internal)?;
        pending.retain(|_, sign_in| sign_in.expires > now);
        if pending.len() >= MAX_PENDING_SIGN_INS {
            return Err(ApiError::new(
                429,
                "Too many pending provider sign-ins. Try again shortly.",
            ));
        }
        pending.insert(
            state.clone(),
            PendingSignIn {
                provider,
                poll_digest: poll_mac(&token).finalize().into_bytes().into(),
                expires: now + SIGN_IN_TTL,
                status: SignInStatus::Waiting(verifier),
            },
        );
        Ok(OAuthStart {
            url,
            poll: OAuthPollRequest { state, token },
        })
    }

    pub(super) fn callback(&self, provider: OAuthProvider, query: &str) -> Result<()> {
        self.complete_callback(provider, query, exchange_identity)
    }

    fn complete_callback(
        &self,
        provider: OAuthProvider,
        query: &str,
        exchange: impl FnOnce(&ProviderConfig, &str, &str) -> Result<ProviderIdentity>,
    ) -> Result<()> {
        let config = self.provider(provider)?;
        let fields = parse_query(query)?;
        let state = fields
            .get("state")
            .filter(|value| valid_token(value))
            .ok_or_else(expired)?;
        if fields
            .get("iss")
            .is_some_and(|issuer| !config.accepts_issuer(issuer))
        {
            return Err(ApiError::new(
                400,
                "The sign-in response came from an unexpected provider.",
            ));
        }
        let verifier = {
            let mut pending = self.pending.lock().map_err(internal)?;
            let sign_in = pending
                .get_mut(state)
                .filter(|sign_in| sign_in.provider == provider && sign_in.expires > Instant::now())
                .ok_or_else(expired)?;
            let SignInStatus::Waiting(_) = &sign_in.status else {
                return Err(expired());
            };
            if fields.contains_key("error") {
                sign_in.status = SignInStatus::Failed;
                return Err(ApiError::new(400, "The provider sign-in was declined."));
            }
            let SignInStatus::Waiting(verifier) =
                std::mem::replace(&mut sign_in.status, SignInStatus::Exchanging)
            else {
                unreachable!("waiting status checked above");
            };
            verifier
        };
        let result = fields
            .get("code")
            .filter(|code| {
                !code.is_empty() && code.len() <= 2048 && !code.chars().any(char::is_control)
            })
            .ok_or_else(|| ApiError::new(400, "The provider did not return a sign-in code."))
            .and_then(|code| exchange(config, code, &verifier));
        let mut pending = self.pending.lock().map_err(internal)?;
        let sign_in = pending
            .get_mut(state)
            .filter(|sign_in| sign_in.expires > Instant::now())
            .ok_or_else(expired)?;
        match result {
            Ok(identity) => {
                sign_in.status = SignInStatus::Ready(identity);
                Ok(())
            }
            Err(error) => {
                sign_in.status = SignInStatus::Failed;
                Err(error)
            }
        }
    }

    pub(super) fn poll(
        &self,
        request: OAuthPollRequest,
        sign_in: impl FnOnce(&ProviderIdentity) -> Result<SignedIn>,
    ) -> Result<OAuthPoll> {
        if !valid_token(&request.state) || !valid_token(&request.token) {
            return Err(expired());
        }
        let mut pending = self.pending.lock().map_err(internal)?;
        let now = Instant::now();
        pending.retain(|_, sign_in| sign_in.expires > now);
        let flow = pending
            .get(&request.state)
            .filter(|flow| {
                poll_mac(&request.token)
                    .verify_slice(&flow.poll_digest)
                    .is_ok()
            })
            .ok_or_else(expired)?;
        match &flow.status {
            SignInStatus::Waiting(_) | SignInStatus::Exchanging => Ok(OAuthPoll::Pending),
            SignInStatus::Ready(identity) => {
                let session = sign_in(identity)?;
                pending.remove(&request.state);
                Ok(OAuthPoll::SignedIn(session))
            }
            SignInStatus::Failed => {
                pending.remove(&request.state);
                Err(ApiError::new(
                    409,
                    "The provider sign-in did not complete. Try again.",
                ))
            }
        }
    }

    fn provider(&self, provider: OAuthProvider) -> Result<&ProviderConfig> {
        self.providers
            .iter()
            .find(|config| config.provider == provider)
            .ok_or_else(|| {
                ApiError::new(
                    503,
                    format!(
                        "{} sign-in is not configured on this records service.",
                        provider.label()
                    ),
                )
            })
    }
}

impl ProviderConfig {
    fn new(
        provider: OAuthProvider,
        client_id: String,
        client_secret: String,
        origin: &str,
    ) -> std::result::Result<Self, String> {
        let origin = origin.trim().trim_end_matches('/');
        let uri: ureq::http::Uri = origin
            .parse()
            .map_err(|_| "Invalid SURF_OAUTH_PUBLIC_URL.")?;
        if uri.scheme_str() != Some("https")
            || uri
                .authority()
                .is_none_or(|authority| authority.as_str().contains('@'))
            || uri.query().is_some()
            || !matches!(uri.path(), "" | "/")
        {
            return Err("SURF_OAUTH_PUBLIC_URL must be an HTTPS origin without a path.".into());
        }
        if [&client_id, &client_secret].into_iter().any(|value| {
            value.is_empty()
                || value.len() > 512
                || !value.bytes().all(|byte| byte.is_ascii_graphic())
        }) {
            return Err(format!(
                "{} OAuth credentials are invalid.",
                provider.label()
            ));
        }
        Ok(Self {
            provider,
            client_id,
            client_secret,
            redirect_uri: format!("{origin}/v1/oauth/{}/callback", provider.id()),
        })
    }

    fn accepts_issuer(&self, issuer: &str) -> bool {
        match self.provider {
            OAuthProvider::Google => matches!(
                issuer,
                "https://accounts.google.com" | "accounts.google.com"
            ),
            OAuthProvider::X => issuer == "https://x.com",
        }
    }
}

fn optional_env(name: &str) -> std::result::Result<Option<String>, String> {
    match std::env::var(name) {
        Ok(value) if !value.is_empty() => Ok(Some(value)),
        Ok(_) | Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(format!("{name} must contain text.")),
    }
}

fn exchange_identity(
    config: &ProviderConfig,
    code: &str,
    verifier: &str,
) -> Result<ProviderIdentity> {
    let (token_url, user_url) = match config.provider {
        OAuthProvider::Google => (
            "https://oauth2.googleapis.com/token",
            "https://openidconnect.googleapis.com/v1/userinfo",
        ),
        OAuthProvider::X => (
            "https://api.x.com/2/oauth2/token",
            "https://api.x.com/2/users/me",
        ),
    };
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(8)))
        .max_redirects(0)
        .build()
        .into();
    let mut body = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("code_verifier", verifier),
        ("redirect_uri", config.redirect_uri.as_str()),
        ("client_id", config.client_id.as_str()),
    ];
    let mut request = agent
        .post(token_url)
        .header("Content-Type", "application/x-www-form-urlencoded");
    match config.provider {
        OAuthProvider::Google => body.push(("client_secret", config.client_secret.as_str())),
        OAuthProvider::X => {
            let credentials = format!(
                "{}:{}",
                encode_part(&config.client_id),
                encode_part(&config.client_secret)
            );
            request = request.header(
                "Authorization",
                format!("Basic {}", STANDARD.encode(credentials)),
            );
        }
    }
    #[derive(Deserialize)]
    struct AccessToken {
        access_token: String,
        token_type: String,
    }
    let token: AccessToken = provider_json(request.send(form(&body)))?;
    if !token.token_type.eq_ignore_ascii_case("bearer")
        || token.access_token.is_empty()
        || token.access_token.len() > 4096
        || !token
            .access_token
            .bytes()
            .all(|byte| byte.is_ascii_graphic())
    {
        return Err(provider_error());
    }
    let response = agent
        .get(user_url)
        .header("Authorization", format!("Bearer {}", token.access_token))
        .call();
    let (subject, name) = match config.provider {
        OAuthProvider::Google => {
            #[derive(Deserialize)]
            struct GoogleProfile {
                sub: String,
                #[serde(default)]
                name: String,
            }
            let profile: GoogleProfile = provider_json(response)?;
            (profile.sub, profile.name)
        }
        OAuthProvider::X => {
            #[derive(Deserialize)]
            struct XProfile {
                data: XUser,
            }
            #[derive(Deserialize)]
            struct XUser {
                id: String,
                username: String,
            }
            let profile: XProfile = provider_json(response)?;
            (profile.data.id, profile.data.username)
        }
    };
    if subject.is_empty()
        || subject.len() > 128
        || !subject.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(provider_error());
    }
    Ok(ProviderIdentity {
        provider: config.provider,
        subject,
        name,
    })
}

fn provider_json<T: DeserializeOwned>(
    response: std::result::Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<T> {
    let mut response = response.map_err(|_| provider_error())?;
    if !response.status().is_success() {
        return Err(provider_error());
    }
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(MAX_PROVIDER_BODY + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| provider_error())?;
    if bytes.len() as u64 > MAX_PROVIDER_BODY {
        return Err(provider_error());
    }
    serde_json::from_slice(&bytes).map_err(|_| provider_error())
}

fn provider_error() -> ApiError {
    ApiError::new(502, "The provider could not verify your sign-in.")
}

fn expired() -> ApiError {
    ApiError::new(404, "This sign-in expired or was already used.")
}

fn valid_token(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn poll_mac(token: &str) -> Hmac<Sha256> {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(token.as_bytes()).expect("HMAC accepts any key length");
    mac.update(POLL_DOMAIN);
    mac
}

fn form(fields: &[(&str, &str)]) -> String {
    fields
        .iter()
        .map(|(key, value)| format!("{}={}", encode_part(key), encode_part(value)))
        .collect::<Vec<_>>()
        .join("&")
}

fn encode_part(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write;
            write!(&mut encoded, "%{byte:02X}").expect("writing to a String cannot fail");
        }
    }
    encoded
}

fn parse_query(query: &str) -> Result<HashMap<String, String>> {
    let invalid = || ApiError::new(400, "Invalid provider sign-in response.");
    if query.len() > 4096 {
        return Err(invalid());
    }
    let mut parsed = HashMap::new();
    for field in query.split('&') {
        let (key, value) = field.split_once('=').ok_or_else(invalid)?;
        let key = decode_part(key).ok_or_else(invalid)?;
        let value = decode_part(value).ok_or_else(invalid)?;
        if parsed.insert(key, value).is_some() {
            return Err(invalid());
        }
    }
    Ok(parsed)
}

fn decode_part(value: &str) -> Option<String> {
    let mut bytes = value.bytes();
    let mut decoded = Vec::with_capacity(value.len());
    while let Some(byte) = bytes.next() {
        let byte = match byte {
            b'+' => b' ',
            b'%' => {
                let high = char::from(bytes.next()?).to_digit(16)?;
                let low = char::from(bytes.next()?).to_digit(16)?;
                (high * 16 + low) as u8
            }
            other => other,
        };
        decoded.push(byte);
    }
    String::from_utf8(decoded).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record_protocol::Profile;

    #[test]
    fn callback_and_poll_are_bound_to_provider_state_pkce_and_client_secret() {
        let config = |provider| {
            ProviderConfig::new(
                provider,
                "test-client".into(),
                "test-secret".into(),
                "https://records.example/",
            )
            .unwrap()
        };
        let oauth = OAuth {
            providers: vec![config(OAuthProvider::Google), config(OAuthProvider::X)],
            pending: Mutex::default(),
        };
        let start = oauth.start(OAuthProvider::Google).unwrap();
        let poll = |token: &str| OAuthPollRequest {
            state: start.poll.state.clone(),
            token: token.to_owned(),
        };
        assert!(matches!(
            oauth
                .poll(poll(&start.poll.token), |_| unreachable!())
                .unwrap(),
            OAuthPoll::Pending
        ));
        let (_, query) = start.url.split_once('?').unwrap();
        let authorization = parse_query(query).unwrap();
        assert_eq!(authorization["code_challenge_method"], "S256");
        assert_eq!(
            authorization["redirect_uri"],
            "https://records.example/v1/oauth/google/callback"
        );
        let callback = format!("state={}&code=test%2Fcode", start.poll.state);
        assert_eq!(
            oauth
                .complete_callback(OAuthProvider::X, &callback, |_, _, _| unreachable!())
                .err()
                .unwrap()
                .status,
            404
        );
        let foreign = format!("{callback}&iss=https%3A%2F%2Fexample.test");
        assert_eq!(
            oauth
                .complete_callback(OAuthProvider::Google, &foreign, |_, _, _| unreachable!())
                .err()
                .unwrap()
                .status,
            400
        );
        oauth
            .complete_callback(
                OAuthProvider::Google,
                &callback,
                |config, code, verifier| {
                    assert_eq!(config.client_secret, "test-secret");
                    assert_eq!(code, "test/code");
                    assert_eq!(
                        authorization["code_challenge"],
                        URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
                    );
                    Ok(ProviderIdentity {
                        provider: OAuthProvider::Google,
                        subject: "verified-subject".into(),
                        name: "Surfer".into(),
                    })
                },
            )
            .unwrap();
        assert!(
            oauth
                .complete_callback(OAuthProvider::Google, &callback, |_, _, _| unreachable!())
                .is_err()
        );
        assert!(
            oauth
                .poll(poll(&"0".repeat(64)), |_| unreachable!())
                .is_err()
        );
        let signed_in = oauth
            .poll(poll(&start.poll.token), |identity| {
                assert_eq!(identity.subject, "verified-subject");
                Ok(SignedIn {
                    token: "local-session".into(),
                    profile: Profile {
                        id: 1,
                        name: "Surfer".into(),
                        motto: String::new(),
                        title: String::new(),
                        banner: String::new(),
                        showcase: Vec::new(),
                        revision: 0,
                        progression: Default::default(),
                        bhop_progression: Default::default(),
                    },
                })
            })
            .unwrap();
        assert!(matches!(signed_in, OAuthPoll::SignedIn(session) if session.profile.id == 1));
        assert!(
            oauth
                .poll(poll(&start.poll.token), |_| unreachable!())
                .is_err()
        );
    }

    #[test]
    fn provider_credentials_require_https_origin() {
        for origin in [
            "http://records.example",
            "https://someone@records.example",
            "https://records.example/path",
        ] {
            assert!(
                ProviderConfig::new(OAuthProvider::X, "id".into(), "secret".into(), origin)
                    .is_err()
            );
        }
    }
}
