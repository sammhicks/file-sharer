use std::{
    future::Future,
    net::{Ipv4Addr, SocketAddr},
};

use askama_axum::IntoResponse as _;
use axum::{
    extract::Multipart,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use axum_extra::routing::RouterExt;

use crate::controller::{Admin, ByteCount, ShareConfig, Token, UploadConfig};

#[derive(askama::Template)]
#[template(path = "admin.html")]
struct HomePage {}

async fn home_page() -> impl IntoResponse {
    HomePage {}.into_response()
}

#[derive(askama::Template)]
#[template(path = "admin_share.html")]
struct SharePage {
    upload_url: String,
    upload_token: Token,
}

async fn new_share(admin: axum::Extension<Admin>) -> Result<impl IntoResponse, StatusCode> {
    let token_config = ShareConfig {
        expiry: chrono::Local::now() + chrono::Duration::hours(1),
    };

    let new_token = admin.new_share_token(token_config).await.map_err(|err| {
        tracing::error!("Failed to create share token: {err}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let url = format!("http://127.0.0.1:8080/share/{new_token}");

    Ok(SharePage {
        upload_url: url,
        upload_token: new_token,
    }
    .into_response())
}

#[derive(axum_extra::routing::TypedPath, serde::Deserialize)]
#[typed_path("/share/:token")]
struct ShareTokenPath {
    token: crate::controller::Token,
}

async fn share_files(
    ShareTokenPath { token }: ShareTokenPath,
    files: Multipart,
    admin: axum::Extension<Admin>,
) -> Result<impl IntoResponse, StatusCode> {
    admin
        .share_files(token, files)
        .await
        .map(|()| "SUCCESS")
        .map_err(|err| {
            tracing::error!("Failed to share files: {err}");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

#[derive(askama::Template)]
#[template(path = "admin_upload.html")]
struct UploadPage {
    upload_url: String,
}

async fn new_upload(admin: axum::Extension<Admin>) -> Result<impl IntoResponse, StatusCode> {
    let token_config = UploadConfig {
        expiry: chrono::Local::now() + chrono::Duration::hours(1),
        space_quota: ByteCount(1_000_000_000),
    };

    let new_token = admin.new_upload_token(token_config).await.map_err(|err| {
        tracing::error!("Failed to create upload token: {err}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let url = format!("http://127.0.0.1:8080/upload/{new_token}");

    Ok(UploadPage { upload_url: url }.into_response())
}

pub async fn run(admin: Admin, shutdown_signal: impl Future<Output = ()>) {
    let app = Router::new()
        .route("/", get(home_page))
        .route("/share", post(new_share))
        .typed_post(share_files)
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
