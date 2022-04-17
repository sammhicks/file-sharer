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
    Admin, ByteCount, ShareConfig, Timestamp, Token, UploadConfig, UploadListing,
};

#[derive(askama::Template)]
#[template(path = "admin.html")]
struct HomePage {
    uploads: Vec<UploadListing>,
    new_upload: NewUpload,
}

async fn home_page(
    admin: axum::extract::Extension<Admin>,
) -> Result<impl IntoResponse, StatusCode> {
    let uploads = admin.current_uploads().await.map_err(|err| {
        tracing::error!("Failed to get current uploads: {err:#}");

        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let new_upload = NewUpload {
        name: String::new(),
        expiry: chrono::Local::now() + chrono::Duration::days(1),
        space_quota: ByteCount(1_000_000_000),
    };

    Ok(HomePage {
        uploads,
        new_upload,
    }
    .into_response())
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
            tracing::error!("Failed to share files: {err}");
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

    let url = format!("http://127.0.0.1:8080/upload/{token}");

    Ok(UploadPage {
        name,
        expiry,
        space_quota,
        upload_url: url,
    }
    .into_response())
}

pub fn html_localtime<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Timestamp, D::Error> {
    use chrono::TimeZone;
    use serde::Deserialize;

    chrono::Local
        .datetime_from_str(
            &String::deserialize(deserializer)?,
            NewUpload::HTML_LOCALTIME,
        )
        .map_err(serde::de::Error::custom)
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct NewUpload {
    name: String,
    #[serde(deserialize_with = "html_localtime")]
    expiry: Timestamp,
    space_quota: ByteCount,
}

impl NewUpload {
    const HTML_LOCALTIME: &'static str = "%FT%H:%M";

    fn expiry_html_localtime(&self) -> impl std::fmt::Display {
        self.expiry.format(Self::HTML_LOCALTIME)
    }
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
    let app = Router::new()
        .route("/", get(home_page))
        .route("/share", post(new_share))
        .typed_post(share_files)
        .typed_get(current_upload)
        .route("/upload/", post(new_upload))
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
