#[cfg(not(feature = "snapshot-http"))]
fn main() {
    eprintln!(
        "snapshot-http feature is not enabled. Rebuild with `--features snapshot-http` to run the HTTP server."
    );
}

#[cfg(feature = "snapshot-http")]
mod server {
    use axum::{
        Json, Router,
        extract::State,
        http::{HeaderMap, StatusCode, header::CONTENT_TYPE},
        routing::post,
    };
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
    use greentic::snapshot::{EnvironmentState, GreenticSnapshot, ValidationPlan, plan_from};
    use serde::{Deserialize, Serialize};
    use std::{env, net::SocketAddr};
    use tokio::net::TcpListener;

    #[derive(Default, Clone)]
    pub struct AppState;

    #[derive(Deserialize)]
    pub struct SnapshotJsonPayload {
        pub gtc_b64: String,
    }

    #[derive(Serialize)]
    pub struct ErrorBody {
        pub error: String,
    }

    pub async fn run() -> anyhow::Result<()> {
        let addr: SocketAddr = env::var("SNAPSHOT_HTTP_ADDR")
            .unwrap_or_else(|_| "0.0.0.0:3000".to_string())
            .parse()
            .expect("Invalid SNAPSHOT_HTTP_ADDR value");

        let app = Router::new()
            .route("/api/snapshot/validate", post(handle_validate))
            .with_state(AppState::default());

        let listener = TcpListener::bind(addr).await?;
        println!("Snapshot validation server listening on {addr}");
        axum::serve(listener, app).await?;
        Ok(())
    }

    async fn handle_validate(
        State(_state): State<AppState>,
        headers: HeaderMap,
        body: axum::body::Bytes,
    ) -> Result<Json<ValidationPlan>, (StatusCode, Json<ErrorBody>)> {
        let content_type = headers
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();

        let snapshot_bytes = if content_type.contains("application/json") {
            let payload: SnapshotJsonPayload = serde_json::from_slice(&body)
                .map_err(|err| bad_request(format!("invalid JSON body: {err}")))?;
            BASE64_STANDARD
                .decode(payload.gtc_b64)
                .map_err(|err| bad_request(format!("invalid base64 data: {err}")))?
        } else {
            if body.is_empty() {
                return Err(bad_request("body is empty".to_string()));
            }
            body.to_vec()
        };

        let snapshot: GreenticSnapshot = serde_cbor::from_slice(&snapshot_bytes)
            .map_err(|err| bad_request(format!("invalid snapshot data: {err}")))?;

        let env_state = EnvironmentState::discover();
        let plan = plan_from(&snapshot.manifest, &env_state);
        Ok(Json(plan))
    }

    fn bad_request(message: String) -> (StatusCode, Json<ErrorBody>) {
        (StatusCode::BAD_REQUEST, Json(ErrorBody { error: message }))
    }
}

#[cfg(feature = "snapshot-http")]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    server::run().await
}
