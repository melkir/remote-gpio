use crate::controller::BlindController;
use crate::embed;
use crate::service::{dispatch_command, CommandError, CommandRequest};
use anyhow::Result;
use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{ConnectInfo, Query, State, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{routing::get, Json, Router};
use futures_util::{
    sink::SinkExt,
    stream::{self, StreamExt},
};
use serde::Deserialize;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use tower_http::trace::DefaultMakeSpan;
use tower_http::trace::TraceLayer;
use tracing::Instrument;

pub(crate) const HTTP_HOST: &str = "127.0.0.1";
pub(crate) const HTTP_PORT: u16 = 5002;

pub(crate) fn base_url() -> String {
    format!("http://{HTTP_HOST}:{HTTP_PORT}")
}

/// Application state shared across all routes
pub struct AppState {
    pub controller: Arc<BlindController>,
}

impl AppState {
    pub fn new(controller: Arc<BlindController>) -> Self {
        Self { controller }
    }
}

/// WebSocket query parameters
#[derive(Debug, Deserialize)]
struct WsQueryParams {
    name: Option<String>,
}

/// Starts the HTTP server with all routes and middleware
pub async fn serve(shared_state: Arc<AppState>) -> Result<()> {
    let app = create_router(shared_state);
    let listener = tokio::net::TcpListener::bind((HTTP_HOST, HTTP_PORT)).await?;
    tracing::info!("Listening on http://{}", listener.local_addr()?);

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}

/// Creates the router with all routes and middleware
fn create_router(shared_state: Arc<AppState>) -> Router {
    Router::new()
        .route("/channel", get(handle_channel))
        .route("/events", get(handle_events))
        .route("/command", post(handle_command))
        .route("/ws", get(ws_handler))
        .fallback(embed::static_handler)
        .with_state(shared_state)
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::default().include_headers(false)),
        )
}

/// Returns the currently-selected channel as plain text.
async fn handle_channel(State(state): State<Arc<AppState>>) -> &'static str {
    state.controller.current_selection().as_str()
}

/// Streams channel selection changes as server-sent events.
async fn handle_events(
    State(state): State<Arc<AppState>>,
) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    let mut rx = state.controller.subscribe_selection();
    rx.mark_changed();
    let stream = stream::unfold(rx, |mut rx| async move {
        rx.changed().await.ok()?;
        let channel = rx.borrow_and_update().as_str();
        Some((Ok(Event::default().event("selection").data(channel)), rx))
    });

    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// Handles command requests via HTTP
async fn handle_command(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<CommandRequest>,
) -> Response {
    match execute_command(&state, payload).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(err) => {
            let status = if err.is_client_error() {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (status, err.to_string()).into_response()
        }
    }
}

async fn execute_command(state: &AppState, payload: CommandRequest) -> Result<(), CommandError> {
    tracing::info!(
        command = %payload.command,
        ?payload.channel,
        ?payload.value,
        "remote command received"
    );
    if let Err(err) = dispatch_command(&state.controller, payload).await {
        tracing::error!(error = %err, "remote command failed");
        return Err(err);
    }
    tracing::info!("remote command completed");
    Ok(())
}

/// Handles WebSocket upgrade requests
async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Query(params): Query<WsQueryParams>,
) -> impl IntoResponse {
    let client = params.name.as_deref().unwrap_or("anonymous");
    // Every log line for this connection, including spawned commands, carries
    // the client identity through this span.
    let span = tracing::info_span!("ws", client, port = addr.port());
    span.in_scope(|| tracing::info!("new WebSocket connection"));
    ws.on_upgrade(move |socket| websocket(socket, state).instrument(span))
}

/// Manages WebSocket connections and message handling
async fn websocket(stream: WebSocket, state: Arc<AppState>) {
    let (mut sink, mut stream) = stream.split();
    let mut rx_channel = state.controller.subscribe_selection();
    let mut ping_interval = tokio::time::interval(std::time::Duration::from_secs(30));

    // Send initial channel state. `borrow_and_update` marks the current value
    // as seen so the first `changed()` reports a real change rather than
    // immediately re-sending what was just written.
    let selection = rx_channel.borrow_and_update().as_str();
    if sink.send(Message::Text(selection.into())).await.is_err() {
        return;
    }

    loop {
        tokio::select! {
            // Send periodic ping to keep connection alive
            _ = ping_interval.tick() => {
                if sink.send(Message::Ping(Bytes::new())).await.is_err() {
                    break;
                }
            }
            // Handle channel state changes.
            result = rx_channel.changed() => {
                if result.is_err() {
                    break;
                }
                let selection = rx_channel.borrow_and_update().as_str();
                if sink.send(Message::Text(selection.into())).await.is_err() {
                    break;
                }
            }
            // Handle incoming messages
            msg = stream.next() => match msg {
                Some(Ok(Message::Text(text))) => spawn_ws_command(&state, &text),
                Some(Ok(_)) => {} // Ignore other message types (Pong, etc.)
                Some(Err(_)) | None => break, // Connection closed or error
            },
        }
    }
}

/// Run a WebSocket text frame as a command without blocking the socket loop.
/// `execute_command` already logs the outcome, so the task discards it.
fn spawn_ws_command(state: &Arc<AppState>, text: &str) {
    let payload = match serde_json::from_str::<CommandRequest>(text) {
        Ok(payload) => payload,
        Err(e) => {
            tracing::error!(error = %e, "invalid JSON command from WebSocket client");
            return;
        }
    };
    let state = Arc::clone(state);
    tokio::spawn(
        async move {
            let _ = execute_command(&state, payload).await;
        }
        .in_current_span(),
    );
}
