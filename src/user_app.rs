use std::{
    future::Future,
    net::{Ipv4Addr, SocketAddr},
};

use askama_axum::IntoResponse as _;
use axum::{extract::Multipart, http::StatusCode, response::IntoResponse, Router, TypedHeader};
use axum_extra::routing::RouterExt;

use crate::controller::User;

#[derive(askama::Template)]
#[template(path = "user_upload.html")]
struct UploadFiles {}

#[derive(axum_extra::routing::TypedPath, serde::Deserialize)]
#[typed_path("/upload/:token")]
struct UploadTokenPath {
    token: crate::controller::Token,
}

async fn upload_files_page(_: UploadTokenPath) -> impl IntoResponse {
    UploadFiles {}.into_response()
}

async fn upload_files(
    UploadTokenPath { token }: UploadTokenPath,
    TypedHeader(content_length): TypedHeader<axum::headers::ContentLength>,
    files: Multipart,
    user: axum::Extension<User>,
) -> impl IntoResponse {
    user.upload_files(token, content_length.0, files)
        .await
        .map(|()| "SUCCESS")
        .map_err(|err| {
            tracing::error!("Failed to upload files: {err:#}");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

#[derive(axum_extra::routing::TypedPath, serde::Deserialize)]
#[typed_path("/share/:token/")]
struct DirectoryListingPath {
    token: crate::controller::Token,
}

async fn directory_listing(
    DirectoryListingPath { token }: DirectoryListingPath,
    user: axum::Extension<User>,
) -> Result<impl IntoResponse, StatusCode> {
    user.directory_listing(token)
        .await
        .map(|listing| listing.into_response())
        .map_err(|err| {
            tracing::error!("{:#}", err);

            StatusCode::NOT_FOUND
        })
}

#[derive(axum_extra::routing::TypedPath, serde::Deserialize)]
#[typed_path("/share/:token/:filename")]
struct SharedFilePath {
    token: crate::controller::Token,
    filename: crate::controller::Filename,
}

async fn share_file(
    SharedFilePath { token, filename }: SharedFilePath,
    user: axum::Extension<User>,
) -> Result<impl IntoResponse, StatusCode> {
    let (file, metadata, mime) = user
        .open_shared_file(token, filename)
        .await
        .map_err(|err| {
            tracing::error!("Could not open shared file: {:#}", err);

            StatusCode::NOT_FOUND
        })?;

    let stream = futures_util::stream::try_unfold(
        tokio::io::BufReader::new(file),
        |mut reader| async move {
            use tokio::io::AsyncBufReadExt;

            let data = reader.fill_buf().await?;
            let data = axum::body::Bytes::copy_from_slice(data);
            reader.consume(data.len());

            Ok::<_, tokio::io::Error>((!data.is_empty()).then(|| (data, reader)))
        },
    );

    let body = axum::body::StreamBody::new(stream);

    Ok((
        StatusCode::OK,
        axum::TypedHeader(axum::headers::ContentType::from(mime)),
        axum::TypedHeader(axum::headers::ContentLength(metadata.len())),
        body,
    )
        .into_response())
}

pub async fn run(user: User, shutdown_signal: impl Future<Output = ()>) {
    let addr = SocketAddr::from((
        if user.config().user_localhost_only {
            Ipv4Addr::LOCALHOST
        } else {
            Ipv4Addr::UNSPECIFIED
        },
        user.config().user_port,
    ));

    let user_root = user.config().user_root.clone();

    let app = Router::new()
        .typed_get(upload_files_page)
        .typed_post(upload_files)
        .typed_get(share_file)
        .typed_get(directory_listing)
        .layer(axum::Extension(user));

    let app = Router::new().nest(&user_root, app);

    tracing::info!("User App is listening on {addr}");

    match axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .with_graceful_shutdown(shutdown_signal)
        .await
    {
        Ok(()) => (),
        Err(err) => tracing::error!("Failed to run user app: {err}"),
    }
}
