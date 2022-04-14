use std::{
    future::Future,
    net::{Ipv4Addr, SocketAddr},
};

use askama_axum::IntoResponse as _;
use axum::{
    extract::Multipart, http::StatusCode, response::IntoResponse, routing::get, Router, TypedHeader,
};
use axum_extra::routing::RouterExt;

use crate::controller::User;

#[derive(askama::Template)]
#[template(path = "user_upload.html")]
struct UploadFiles {}

#[derive(askama::Template)]
#[template(path = "user_upload_success.html")]
struct UploadFilesSuccess {
    message: String,
}

#[derive(axum_extra::routing::TypedPath, serde::Deserialize)]
#[typed_path("/upload/:token")]
struct UploadToken {
    token: crate::controller::Token,
}

async fn upload_files_page(_: UploadToken) -> impl IntoResponse {
    UploadFiles {}.into_response()
}

async fn upload_files(
    UploadToken { token }: UploadToken,
    TypedHeader(content_length): TypedHeader<axum::headers::ContentLength>,
    files: Multipart,
    user: axum::Extension<User>,
) -> impl IntoResponse {
    user.upload_files(token, content_length.0, files)
        .await
        .map(|()| {
            UploadFilesSuccess {
                message: "SUCCESS".into(),
            }
            .into_response()
        })
        .map_err(|err| {
            tracing::error!("Failed to upload files: {err:#}");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

pub async fn run(user: User, shutdown_signal: impl Future<Output = ()>) {
    let app = Router::new()
        .route("/", get(|| async { "Hello, World!" }))
        .typed_get(upload_files_page)
        .typed_post(upload_files)
        .layer(axum::Extension(user));

    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, 8080));

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
