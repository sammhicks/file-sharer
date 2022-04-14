use std::{
    future::Future,
    net::{Ipv4Addr, SocketAddr},
};

use askama_axum::IntoResponse as _;
use axum::{
    response::IntoResponse,
    routing::{get, post},
    Router,
};

use crate::controller::Admin;

#[derive(askama::Template)]
#[template(path = "admin.html")]
struct HomePage {}

async fn home_page() -> impl IntoResponse {
    HomePage {}.into_response()
}

#[derive(askama::Template)]
#[template(path = "admin_upload.html")]
struct UploadPage {
    upload_url: String,
}

async fn new_upload(admin: axum::Extension<Admin>) -> impl IntoResponse {
    let new_token = admin.new_upload_token().await;

    let url = format!("http://127.0.0.1:8080/upload/{new_token}");

    UploadPage { upload_url: url }.into_response()
}

pub async fn run(admin: Admin, shutdown_signal: impl Future<Output = ()>) {
    let app = Router::new()
        .route("/", get(home_page))
        .route("/upload", post(new_upload))
        .layer(axum::Extension(admin));

    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, 8000));

    tracing::info!("Admin App is listening on {addr}");

    match axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .with_graceful_shutdown(shutdown_signal)
        .await
    {
        Ok(()) => (),
        Err(err) => tracing::error!("Failed to run admin app: {err}"),
    }
}
