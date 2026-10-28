// SPDX-License-Identifier: MPL-2.0
//! Puggle command-line entry point and untrusted HTTP boundary.

use std::env;
use std::error::Error;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use puggle::ast::{Binding, FormalAst, Sort, TargetProver};
use puggle::gatekeeper::{EnglishDeformaliser, JaccardMetric, SemanticGatekeeper};
use puggle::wire;
use serde::{Deserialize, Serialize};

#[derive(Clone)]
struct AppState {
    gatekeeper: Arc<SemanticGatekeeper>,
}

#[derive(Debug, Deserialize)]
struct FreezeRequest {
    original: String,
    candidate: Candidate,
}

#[derive(Debug, Deserialize)]
struct Candidate {
    target: TargetProver,
    name: String,
    bindings: Vec<Binding>,
    result: Sort,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: String,
}

#[derive(Debug, Serialize)]
struct Health {
    status: &'static str,
    echidna_tcb: bool,
}

/// Run the CLI, printing failures and terminating the process with exit code 1.
#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("puggle: {error}");
        std::process::exit(1);
    }
}

/// Serve on `serve`, print help for help flags or no command, and reject unknown
/// commands. Server configuration and I/O errors propagate to the entry point.
async fn run() -> Result<(), Box<dyn Error>> {
    let mut arguments = env::args().skip(1);
    match arguments.next().as_deref() {
        Some("serve") => serve().await,
        Some("help" | "--help" | "-h") | None => {
            println!(
                "Puggle — untrusted neurosymbolic formalisation front-end\n\
                 \n\
                 USAGE:\n    \
                 puggle serve\n\
                 \n\
                 ENVIRONMENT:\n    \
                 PUGGLE_ADDR    Listen address (default 0.0.0.0:3000)\n\
                 \n\
                 POST /v1/freeze audits an original text and untrusted candidate AST.\n\
                 Successful responses are Cap'n Proto; rejected candidates never reach ECHIDNA."
            );
            Ok(())
        }
        Some(command) => Err(format!("unknown command {command:?}; use `puggle help`").into()),
    }
}

/// Serve health and freeze routes with three reference agents and a lexical
/// metric, limiting request bodies to 128 KiB. `PUGGLE_ADDR` sets the socket
/// address; an absent or non-Unicode value falls back to `127.0.0.1:3000`.
/// Return address parsing, gatekeeper construction, bind, or serving errors.
async fn serve() -> Result<(), Box<dyn Error>> {
    let address = env::var("PUGGLE_ADDR").unwrap_or_else(|_| "127.0.0.1:3000".to_owned());
    let address: SocketAddr = address.parse()?;
    let agents = (0..3)
        .map(|style| {
            Arc::new(EnglishDeformaliser::new(style)) as Arc<dyn puggle::gatekeeper::Deformaliser>
        })
        .collect();
    let gatekeeper = Arc::new(SemanticGatekeeper::new(
        agents,
        Arc::new(JaccardMetric::lexical()),
        3,
    )?);
    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/freeze", post(freeze))
        .layer(DefaultBodyLimit::max(128 * 1024))
        .with_state(AppState { gatekeeper });

    let listener = tokio::net::TcpListener::bind(address).await?;
    eprintln!("puggle: listening on http://{address} (outside ECHIDNA's TCB)");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

/// Wait for Ctrl-C; failure to install the signal handler also completes the
/// future, allowing the server's graceful shutdown to proceed.
async fn shutdown() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        eprintln!("puggle: shutdown signal failed: {error}");
    }
}

/// Report liveness and that this service is outside ECHIDNA's trusted base.
async fn health() -> Json<Health> {
    Json(Health {
        status: "ok",
        echidna_tcb: false,
    })
}

/// Validate and audit a candidate once, returning a Cap'n Proto handoff with
/// its lowercase hexadecimal AST digest in `x-puggle-ast-hash` on success.
/// AST or audit failures become JSON errors with status 422; wire or digest
/// header encoding failures become JSON errors with status 500.
async fn freeze(State(state): State<AppState>, Json(request): Json<FreezeRequest>) -> Response {
    let candidate = request.candidate;
    let ast = match FormalAst::new(
        candidate.target,
        candidate.name,
        candidate.bindings,
        candidate.result,
    ) {
        Ok(ast) => ast,
        Err(error) => return error_response(StatusCode::UNPROCESSABLE_ENTITY, error.to_string()),
    };

    match state
        .gatekeeper
        .audit_candidate(&request.original, ast)
        .await
    {
        Ok(frozen) => match wire::encode(&frozen) {
            Ok(bytes) => {
                let mut response = bytes.into_response();
                response.headers_mut().insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/x-capnp"),
                );
                let digest = hex(frozen.ast_hash());
                match HeaderValue::from_str(&digest) {
                    Ok(value) => {
                        response.headers_mut().insert("x-puggle-ast-hash", value);
                        response
                    }
                    Err(error) => error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("failed to encode digest header: {error}"),
                    ),
                }
            }
            Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
        },
        Err(rejection) => error_response(StatusCode::UNPROCESSABLE_ENTITY, rejection.to_string()),
    }
}

fn error_response(status: StatusCode, message: String) -> Response {
    (status, Json(ErrorBody { error: message })).into_response()
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}
