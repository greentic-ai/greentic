use axum::extract::Path;
use axum::response::IntoResponse;
use axum::Extension;
use axum::{extract::Query, response::Redirect};
use channel_plugin::plugin_runtime::PluginHandler;
use hyper::StatusCode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::sync::RwLock;
use lazy_static::lazy_static;
use openidconnect::core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata};
use openidconnect::{OAuth2TokenResponse, AccessTokenHash, AuthorizationCode, ClientId, ClientSecret, CsrfToken, IssuerUrl, Nonce, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope, TokenResponse};
use crate::WebhookPlugin;
use anyhow::anyhow;

pub type OICClient = openidconnect::Client<openidconnect::EmptyAdditionalClaims, openidconnect::core::CoreAuthDisplay, openidconnect::core::CoreGenderClaim, openidconnect::core::CoreJweContentEncryptionAlgorithm, openidconnect::core::CoreJsonWebKey, openidconnect::core::CoreAuthPrompt, openidconnect::StandardErrorResponse<openidconnect::core::CoreErrorResponseType>, openidconnect::StandardTokenResponse<openidconnect::IdTokenFields<openidconnect::EmptyAdditionalClaims, openidconnect::EmptyExtraTokenFields, openidconnect::core::CoreGenderClaim, openidconnect::core::CoreJweContentEncryptionAlgorithm, openidconnect::core::CoreJwsSigningAlgorithm>, openidconnect::core::CoreTokenType>, openidconnect::StandardTokenIntrospectionResponse<openidconnect::EmptyExtraTokenFields, openidconnect::core::CoreTokenType>, openidconnect::core::CoreRevocableToken, openidconnect::StandardErrorResponse<openidconnect::RevocationErrorResponseType>, openidconnect::EndpointSet, openidconnect::EndpointNotSet, openidconnect::EndpointNotSet, openidconnect::EndpointNotSet, openidconnect::EndpointMaybeSet, openidconnect::EndpointMaybeSet>;

lazy_static! {
    static ref OAUTH_STATE_STORE: RwLock<HashMap<String, (CsrfToken, Nonce, String)>> =
        RwLock::new(HashMap::new());
}

#[derive(Debug, Deserialize)]
pub struct OAuthCallback {
    pub code: String,
    pub state: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct OAuthProvider {
    pub name: String,
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    pub token_url: String,
    pub user_info_url: String,
    pub auth_url: Option<String>, // Custom authorization URL
}

pub async fn start_oauth(    
    plugin: WebhookPlugin,
    provider: &str,
    session_id: String) -> anyhow::Result<Redirect> {
    let client_id = plugin
        .get_secret(&format!("{}_CLIENT_ID", provider.to_uppercase()))
        .ok_or_else(|| anyhow::anyhow!("Missing client ID for {}", provider))?;

    let client_secret = plugin
        .get_secret(&format!("{}_CLIENT_SECRET", provider.to_uppercase()))
        .ok_or_else(|| anyhow::anyhow!("Missing client secret for {}", provider))?;

    let issuer_url_str = plugin
        .get_config(&format!("{}_ISSUER_URL", provider.to_uppercase()))
        .ok_or_else(|| anyhow::anyhow!("Missing issuer URL for {}", provider))?;

    let redirect_domain = plugin
        .get_config("OAUTH_REDIRECT_DOMAIN")
        .unwrap_or_else(|| "http://localhost:3000".into());

    let redirect_url = format!("{}/auth/callback", redirect_domain);
    // Build a safe HTTP client for OpenID discovery
    let http_client = reqwest::ClientBuilder::new()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("Client should build");

    let provider_metadata = CoreProviderMetadata::discover_async(
        IssuerUrl::new(issuer_url_str)?,
        &http_client,
    )
    .await?;

    let client = CoreClient::from_provider_metadata(
        provider_metadata,
        ClientId::new(client_id),
        Some(ClientSecret::new(client_secret)),
    )
    .set_redirect_uri(RedirectUrl::new(redirect_url)?);

    let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();
    let csrf_token = CsrfToken::new_random();
    let nonce = Nonce::new_random();

    let csrf_token_for_closure = csrf_token.clone();
    let nonce_for_closure = nonce.clone();
    let (auth_url, _, _) = client
        .authorize_url(
            CoreAuthenticationFlow::AuthorizationCode,
            move || csrf_token_for_closure,
            move || nonce_for_closure,
        )
        .add_scope(Scope::new("openid".into()))
        .add_scope(Scope::new("email".into()))
        .add_scope(Scope::new("profile".into()))
        .set_pkce_challenge(pkce_challenge.clone())
        .url();

    OAUTH_STATE_STORE
        .write()
        .await
        .insert(session_id.clone(), (csrf_token.clone(), nonce.clone(), pkce_verifier.secret().to_string()));

    Ok(Redirect::to(auth_url.as_ref()))
}

pub async fn oauth_callback(
    Path(provider): Path<String>,
    Query(query): Query<OAuthCallback>,
    Extension(plugin): Extension<WebhookPlugin>,
) -> impl IntoResponse {
    match actual_oauth_callback(query, plugin, &provider).await {
        Ok(text) => text.into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("OAuth error: {}", e),
        ).into_response(),
    }
}

pub async fn actual_oauth_callback(
    Query(query): Query<OAuthCallback>,
    session_id: String,
    plugin: WebhookPlugin,
    provider: &str,
) -> anyhow::Result<String> {
    let http_client = reqwest::ClientBuilder::new()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("Client should build");

    let (client_id, client_secret, redirect_url, issuer_url) =
        get_client_config(plugin, provider)?;

    let provider_metadata =
        CoreProviderMetadata::discover_async(issuer_url, &http_client).await?;

    let client = CoreClient::from_provider_metadata(
        provider_metadata,
        client_id,
        Some(client_secret),
    )
    .set_redirect_uri(redirect_url);

    let (stored_csrf, nonce, pkce_verifier) = OAUTH_STATE_STORE
        .write()
        .await
        .remove(&session_id)
        .ok_or_else(|| anyhow!("Missing state/session"))?;

    if query.state != *stored_csrf.secret() {
        return Err(anyhow!("CSRF token mismatch"));
    }

    let token_response = client
        .exchange_code(AuthorizationCode::new(query.code))
        .unwrap()
        .set_pkce_verifier(PkceCodeVerifier::new(pkce_verifier))
        .request_async(&http_client)
        .await?;

    let id_token = token_response
        .id_token()
        .ok_or_else(|| anyhow!("No ID token"))?;

    let id_token_verifier = client.id_token_verifier();
    let claims = id_token.claims(&id_token_verifier, &nonce)?;

    if let Some(expected_hash) = claims.access_token_hash() {
        let actual_hash = AccessTokenHash::from_token(
            token_response.access_token(),
            id_token.signing_alg()?,
            id_token.signing_key(&id_token_verifier)?,
        )?;
        if actual_hash != *expected_hash {
            return Err(anyhow!("Access token hash mismatch"));
        }
    }

    Ok(format!(
        "User {} authenticated successfully (email: {})",
        claims.subject().as_str(),
        claims
            .email()
            .map(|e| e.as_str())
            .unwrap_or("<not provided>")
    ))
}

pub async fn init_oidc_client(
    client_id: String,
    client_secret: String,
    redirect_uri: String,
    issuer: String,
) -> anyhow::Result<OICClient> {
    let http_client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;

    let provider_metadata = CoreProviderMetadata::discover_async(
        IssuerUrl::new(issuer)?,
        &http_client,
    )
    .await?;

    let client  = CoreClient::from_provider_metadata(
        provider_metadata,
        ClientId::new(client_id),
        Some(ClientSecret::new(client_secret)),
    )
    .set_redirect_uri(RedirectUrl::new(redirect_uri)?);

    Ok(client)
}

fn get_client_config(
    plugin: WebhookPlugin,
    provider: &str,
) -> anyhow::Result<(ClientId, ClientSecret, RedirectUrl, IssuerUrl)> {
    let client_id = plugin
        .get_secret(&format!("{}_CLIENT_ID", provider.to_uppercase()))
        .ok_or_else(|| anyhow!("Missing client ID"))?;
    let client_secret = plugin
        .get_secret(&format!("{}_CLIENT_SECRET", provider.to_uppercase()))
        .ok_or_else(|| anyhow!("Missing client secret"))?;

    let domain = plugin
        .get_config("OAUTH_REDIRECT_DOMAIN")
        .unwrap_or_else(|| "http://localhost:3000".into());
    let redirect_uri = format!("{}/auth/callback", domain);

    let issuer_url = match provider {
        "google" => "https://accounts.google.com".into(),
        "github" => "https://github.com".into(), // GitHub note: fallback only, not pure OIDC
        _ => plugin
            .get_config(&format!("{}_ISSUER", provider.to_uppercase()))
            .ok_or_else(|| anyhow!("Missing issuer URL"))?,
    };

    Ok((
        ClientId::new(client_id),
        ClientSecret::new(client_secret),
        RedirectUrl::new(redirect_uri)?,
        IssuerUrl::new(issuer_url)?,
    ))
}


#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::MockServer;
    use httpmock::Method::GET;
    use serde_json::json;

   #[tokio::test]
    async fn test_init_oidc_client_with_mock_metadata() {
        let server = MockServer::start();

        let metadata = json!({
            "issuer": server.url("/"),
            "authorization_endpoint": server.url("/authorize"),
            "token_endpoint": server.url("/token"),
            "jwks_uri": server.url("/jwks"),
            "response_types_supported": ["code"],
            "subject_types_supported": ["public"],
            "id_token_signing_alg_values_supported": ["RS256"]
        });

        let mock = server.mock(|when, then| {
            when.method(GET).path("/");
            then.status(200).json_body(metadata);
        });

        let client = init_oidc_client(
            "client123".into(),
            "secret456".into(),
            "https://myapp.com/callback".into(),
            server.url("/"),
        )
        .await
        .expect("Client should be initialized");

        mock.assert();

        let (auth_url, _csrf_token, _nonce) = client
            .authorize_url(
                CoreAuthenticationFlow::AuthorizationCode,
                CsrfToken::new_random,
                Nonce::new_random,
            )
            .add_scope(Scope::new("openid".into()))
            .url();

        assert!(auth_url.to_string().starts_with(&server.url("/authorize")));
    }
}