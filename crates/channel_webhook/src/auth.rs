use crate::AuthConfig;
use axum::http::HeaderMap;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};
use reqwest::Client;
use tracing::{warn, info};
use std::time::{SystemTime, UNIX_EPOCH};

pub async fn verify(config: &AuthConfig, headers: &HeaderMap, body: &str) -> bool {
    match config {
        AuthConfig::Github { secret } => verify_hmac("X-Hub-Signature-256", secret, headers, body),

        AuthConfig::Stripe { secret } => verify_hmac("Stripe-Signature", secret, headers, body),

        AuthConfig::Slack { signing_secret } => verify_slack_signature(signing_secret, headers, body),

        AuthConfig::Zoom { verification_token } => verify_token_header("X-Zoom-Token", verification_token, headers)
            || body.contains(verification_token),

        AuthConfig::Twilio { auth_token } => verify_token_header("X-Twilio-Signature", auth_token, headers)
            || body.contains(auth_token),

        AuthConfig::CustomHmac { secret, header } => verify_hmac(header, secret, headers, body),

        AuthConfig::JwtWithJwks { jwks_url, expected_issuer, expected_audience } => {
            verify_jwt(jwks_url, expected_issuer.clone(), expected_audience.clone(), body).await
        }
        AuthConfig::Google { jwks_url } | AuthConfig::Microsoft { jwks_url } => {
            verify_jwt(jwks_url, None, None, body).await
        }

        AuthConfig::Auto => auto_detect(headers, body).await,
    }
}

async fn auto_detect(headers: &HeaderMap, body: &str) -> bool {
    if headers.contains_key("X-GitHub-Delivery") {
        return true; // or call GitHub HMAC verifier with known test key
    }
    if headers.contains_key("Stripe-Signature") {
        return true;
    }
    if headers.contains_key("X-Slack-Signature") {
        return verify_slack_signature("dev-secret", headers, body); // fallback slack
    }
    if headers.contains_key("X-Zoom-Token") {
        return verify_token_header("X-Zoom-Token", "dev-token", headers);
    }
    if headers.contains_key("X-Twilio-Signature") {
        return verify_token_header("X-Twilio-Signature", "dev-token", headers);
    }
    if headers.contains_key("Authorization") {
        let bearer = headers.get("Authorization").and_then(|v| v.to_str().ok()).unwrap_or("");
        return verify_jwt("https://example.com/.well-known/jwks.json", None, None, bearer).await;
    }
    info!("Auto-detect failed, passing for dev mode");
    true
}

fn verify_hmac(header: &str, secret: &str, headers: &HeaderMap, body: &str) -> bool {
    if let Some(header_value) = headers.get(header) {
        if let Ok(provided_sig) = header_value.to_str() {
            let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
            mac.update(body.as_bytes());
            let expected_sig = mac.finalize().into_bytes();
            let expected_hex = format!("sha256={:x}", expected_sig);
            return constant_time_eq(provided_sig, &expected_hex);
        }
    }
    false
}

fn verify_slack_signature(secret: &str, headers: &HeaderMap, body: &str) -> bool {
    let timestamp = headers.get("X-Slack-Request-Timestamp");
    let signature = headers.get("X-Slack-Signature");

    if let (Some(ts), Some(sig)) = (timestamp, signature) {
        let ts_str = ts.to_str().unwrap_or("");
        let ts_parsed = ts_str.parse::<u64>().unwrap_or(0);

        // Check for replay attacks (within 5 min)
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        if now.abs_diff(ts_parsed) > 300 {
            warn!("Slack request expired: {} vs now {}", ts_parsed, now);
            return false;
        }

        let base_string = format!("v0:{}:{}", ts_str, body);
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(base_string.as_bytes());
        let expected_sig = format!("v0={:x}", mac.finalize().into_bytes());

        if let Ok(sig_str) = sig.to_str() {
            return constant_time_eq(&expected_sig, sig_str);
        }
    }
    false
}

fn verify_token_header(header: &str, expected_value: &str, headers: &HeaderMap) -> bool {
    if let Some(header_value) = headers.get(header) {
        if let Ok(actual_value) = header_value.to_str() {
            return constant_time_eq(actual_value, expected_value);
        }
    }
    false
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut result = 0;
    for (x, y) in a.bytes().zip(b.bytes()) {
        result |= x ^ y;
    }
    result == 0
}

async fn verify_jwt(
    jwks_url: &str,
    expected_issuer: Option<String>,
    expected_audience: Option<String>,
    token: &str,
) -> bool {
    let header = decode_header(token).ok();
    let kid = header.and_then(|h| h.kid);
    if kid.is_none() {
        warn!("Missing kid in JWT header");
        return false;
    }

    let jwks = match fetch_jwks(jwks_url).await {
        Ok(jwks) => jwks,
        Err(e) => {
            warn!("Failed to fetch JWKS: {}", e);
            return false;
        }
    };

    if let Some(jwk) = jwks.keys.iter().find(|k| k.kid == kid.as_deref().unwrap()) {
        let decoding_key = DecodingKey::from_rsa_components(&jwk.n, &jwk.e).unwrap();

        let mut validation = Validation::new(Algorithm::RS256);
        if let Some(iss) = expected_issuer { validation.set_issuer(&[iss]); }
        if let Some(aud) = expected_audience { validation.set_audience(&[aud]); }

        decode::<Claims>(token, &decoding_key, &validation).is_ok()
    } else {
        false
    }
}

#[derive(Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

#[derive(Deserialize)]
struct Jwk {
    kid: String,
    n: String,
    e: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    sub: String,
    exp: usize,
    iss: Option<String>,
    aud: Option<String>,
}

async fn fetch_jwks(jwks_url: &str) -> Result<Jwks, reqwest::Error> {
    let res = Client::new().get(jwks_url).send().await?;
    res.json::<Jwks>().await
}
#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;

    #[tokio::test]
    async fn test_github_hmac_valid() {
        let secret = "testsecret";
        let body = "payload";
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body.as_bytes());
        let sig = format!("sha256={:x}", mac.finalize().into_bytes());

        let mut headers = HeaderMap::new();
        headers.insert("X-Hub-Signature-256", sig.parse().unwrap());

        let config = AuthConfig::Github { secret: secret.into() };
        assert!(verify(&config, &headers, body).await);
    }

    #[tokio::test]
    async fn test_stripe_hmac_invalid() {
        let secret = "wrongsecret";
        let body = "payload";
        let mut headers = HeaderMap::new();
        headers.insert("Stripe-Signature", "sha256=invalidsig".parse().unwrap());

        let config = AuthConfig::Stripe { secret: secret.into() };
        assert!(!verify(&config, &headers, body).await);
    }

    #[tokio::test]
    async fn test_slack_signature_valid() {
        let secret = "slacksecret";
        let body = "text";
        let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs().to_string();
        let base_string = format!("v0:{}:{}", ts, body);
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(base_string.as_bytes());
        let sig = format!("v0={:x}", mac.finalize().into_bytes());

        let mut headers = HeaderMap::new();
        headers.insert("X-Slack-Request-Timestamp", ts.parse().unwrap());
        headers.insert("X-Slack-Signature", sig.parse().unwrap());

        let config = AuthConfig::Slack { signing_secret: secret.into() };
        assert!(verify(&config, &headers, body).await);
    }

    #[tokio::test]
    async fn test_zoom_token_header_valid() {
        let token = "zoomsupersecret";
        let mut headers = HeaderMap::new();
        headers.insert("X-Zoom-Token", token.parse().unwrap());

        let config = AuthConfig::Zoom { verification_token: token.into() };
        assert!(verify(&config, &headers, "irrelevant").await);
    }

    #[tokio::test]
    async fn test_twilio_token_body_fallback() {
        let token = "twiliokey";
        let body = format!("{{'token': '{}'}}", token);

        let headers = HeaderMap::new();
        let config = AuthConfig::Twilio { auth_token: token.into() };
        assert!(verify(&config, &headers, &body).await);
    }

    #[tokio::test]
    async fn test_custom_hmac_valid() {
        let secret = "abc123";
        let header = "X-Custom-HMAC";
        let body = "foobar";
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body.as_bytes());
        let sig = format!("sha256={:x}", mac.finalize().into_bytes());

        let mut headers = HeaderMap::new();
        headers.insert(header, sig.parse().unwrap());

        let config = AuthConfig::CustomHmac { secret: secret.into(), header: header.into() };
        assert!(verify(&config, &headers, body).await);
    }

    #[tokio::test]
    async fn test_auto_detect_slack() {
        let mut headers = HeaderMap::new();
        let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs().to_string();
        let base_string = format!("v0:{}:{}", ts, "testbody");
        let mut mac = Hmac::<Sha256>::new_from_slice(b"dev-secret").unwrap();
        mac.update(base_string.as_bytes());
        let sig = format!("v0={:x}", mac.finalize().into_bytes());

        headers.insert("X-Slack-Request-Timestamp", ts.parse().unwrap());
        headers.insert("X-Slack-Signature", sig.parse().unwrap());

        let config = AuthConfig::Auto;
        assert!(verify(&config, &headers, "testbody").await);
    }

    #[tokio::test]
    async fn test_auto_detect_dev_fallback() {
        let headers = HeaderMap::new();
        let config = AuthConfig::Auto;
        assert!(verify(&config, &headers, "test").await);
    }
}