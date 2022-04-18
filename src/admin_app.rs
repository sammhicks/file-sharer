use std::{
    future::Future,
    net::{Ipv4Addr, SocketAddr},
};

use askama_axum::IntoResponse as _;
use axum::{
    extract::{Form, Multipart},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use axum_extra::routing::{RouterExt, TypedPath};

use crate::controller::{
    Admin, ByteCount, ShareConfig, ShareListing, Timestamp, Token, UploadConfig, UploadListing,
};

#[derive(askama::Template)]
#[template(path = "admin.html")]
struct HomePage {
    shares: Vec<ShareListing>,
    new_share: NewShare,
    uploads: Vec<UploadListing>,
    new_upload: NewUpload,
}

async fn home_page(
    admin: axum::extract::Extension<Admin>,
) -> Result<impl IntoResponse, StatusCode> {
    let shares = admin.current_shares().await.map_err(|err| {
        tracing::error!("Failed to get current shares: {err:#}");

        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let new_share = NewShare {
        name: String::new(),
        expiry: Timestamp::now() + chrono::Duration::days(1),
    };

    let uploads = admin.current_uploads().await.map_err(|err| {
        tracing::error!("Failed to get current uploads: {err:#}");

        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let new_upload = NewUpload {
        name: String::new(),
        expiry: Timestamp::now() + chrono::Duration::days(1),
        space_quota: ByteCount(1_000_000_000),
    };

    Ok(HomePage {
        shares,
        new_share,
        uploads,
        new_upload,
    }
    .into_response())
}

#[derive(TypedPath, serde::Deserialize)]
#[typed_path("/share/:token")]
struct SharePagePath {
    token: Token,
}

#[derive(askama::Template)]
#[template(path = "admin_share.html")]
struct SharePage {
    name: String,
    expiry: Timestamp,
    upload_url: String,
}

async fn current_share(
    SharePagePath { token }: SharePagePath,
    admin: axum::Extension<Admin>,
) -> Result<impl IntoResponse, StatusCode> {
    let ShareConfig { name, expiry } = admin.current_share_config(&token).await.map_err(|err| {
        tracing::error!("{err:#}");

        StatusCode::NOT_FOUND
    })?;

    let user_root = &admin.config().user_root;
    let url = format!("{user_root}/share/{token}");

    Ok(SharePage {
        name,
        expiry,
        upload_url: url,
    }
    .into_response())
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct NewShare {
    name: String,
    expiry: Timestamp,
}

async fn new_share(
    Form(NewShare { name, expiry }): Form<NewShare>,
    admin: axum::Extension<Admin>,
) -> Result<impl IntoResponse, StatusCode> {
    let new_token = admin
        .new_share_token(ShareConfig { name, expiry })
        .await
        .map_err(|err| {
            tracing::error!("Failed to create share token: {err:#}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(axum::response::Redirect::to(new_token.as_str()))
}

#[derive(TypedPath, serde::Deserialize)]
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
            tracing::error!("Failed to share files: {err:#}");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

#[derive(TypedPath, serde::Deserialize)]
#[typed_path("/upload/:token")]
struct UploadPagePath {
    token: Token,
}

#[derive(askama::Template)]
#[template(path = "admin_upload.html")]
struct UploadPage {
    name: String,
    expiry: Timestamp,
    space_quota: ByteCount,
    upload_url: String,
}

async fn current_upload(
    UploadPagePath { token }: UploadPagePath,
    admin: axum::Extension<Admin>,
) -> Result<impl IntoResponse, StatusCode> {
    let UploadConfig {
        name,
        expiry,
        space_quota,
    } = admin.current_upload_config(&token).await.map_err(|err| {
        tracing::error!("{err:#}");

        StatusCode::NOT_FOUND
    })?;

    let user_root = &admin.config().user_root;
    let url = format!("{user_root}/upload/{token}");

    Ok(UploadPage {
        name,
        expiry,
        space_quota,
        upload_url: url,
    }
    .into_response())
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct NewUpload {
    name: String,
    expiry: Timestamp,
    space_quota: ByteCount,
}

async fn new_upload(
    Form(NewUpload {
        name,
        expiry,
        space_quota,
    }): Form<NewUpload>,
    admin: axum::Extension<Admin>,
) -> Result<impl IntoResponse, StatusCode> {
    let new_token = admin
        .new_upload_token(UploadConfig {
            name,
            expiry,
            space_quota,
        })
        .await
        .map_err(|err| {
            tracing::error!("Failed to create upload token: {err}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(axum::response::Redirect::to(new_token.as_str()))
}

pub async fn run(admin: Admin, shutdown_signal: impl Future<Output = ()>) {
    if admin.config().disable_admin_app {
        shutdown_signal.await;

        return;
    }

    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, admin.config().admin_port));

    let app = Router::new()
        .route("/", get(home_page))
        .typed_get(current_share)
        .route("/share/", post(new_share))
        .typed_post(share_files)
        .typed_get(current_upload)
        .route("/upload/", post(new_upload))
        .layer(axum::Extension(admin));

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
