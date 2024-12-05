use axum::{
    extract::{Form, Json, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use hyper_util::client::legacy::connect::HttpConnector;
use pusher::Pusher;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use tower_http::services::ServeDir;

use crate::remote::{Command, Input, RemoteControl};

pub async fn start_server(rc: Arc<RemoteControl>, pusher: Arc<Pusher<HttpConnector>>) {
    let app = Router::new()
        .route("/pusher/auth", post(pusher_auth))
        .route("/command", post(handle_command))
        .route("/led", get(get_led))
        .nest_service("/", ServeDir::new("assets"))
        .with_state((rc, pusher));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();

    println!("Server started on 0.0.0.0:3000");

    axum::serve(listener, app).await.unwrap();
}

async fn pusher_auth(
    State((_, pusher)): State<(Arc<RemoteControl>, Arc<Pusher<HttpConnector>>)>,
    Form(params): Form<HashMap<String, String>>,
) -> impl IntoResponse {
    println!("Pusher auth called");
    let channel_name = params
        .get("channel_name")
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "Missing channel_name".to_string()))?;
    let socket_id = params
        .get("socket_id")
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "Missing socket_id".to_string()))?;

    println!("Channel name: {}", channel_name);
    println!("Socket ID: {}", socket_id);

    let auth_signature = pusher.authenticate_private_channel(channel_name, socket_id);

    match auth_signature {
        Ok(auth) => {
            let json = Json(auth);
            println!("Auth signature: {:?}", json);
            Ok((StatusCode::OK, json))
        }
        Err(e) => Err((StatusCode::UNAUTHORIZED, e.to_string())),
    }
}

#[derive(Deserialize)]
struct CommandRequest {
    command: String,
    led: Option<Input>,
}

async fn handle_command(
    State((rc, _)): State<(Arc<RemoteControl>, Arc<Pusher<HttpConnector>>)>,
    Json(payload): Json<CommandRequest>,
) -> impl IntoResponse {
    println!(
        "Received command: {:?} and led: {:?}",
        payload.command, payload.led
    );
    match Command::from_str(&payload.command) {
        Some(command) => {
            tokio::spawn(async move {
                rc.send(command, payload.led).await;
            });
            (StatusCode::OK, "ok")
        }
        None => (StatusCode::BAD_REQUEST, "Invalid command"),
    }
}

async fn get_led(state: State<(Arc<RemoteControl>, Arc<Pusher<HttpConnector>>)>) -> String {
    println!("Get led called");
    let (rc, _) = &*state;
    let selection = rc.selection.lock().unwrap().clone();
    println!("Selection: {}", selection);
    selection
}
