use std::{
    fmt,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{extract::Multipart, http::StatusCode};
use futures_util::StreamExt;

fn assert_crypto_secure<R: rand::CryptoRng>(r: R) -> R {
    r
}

fn sanitize_path<P: AsRef<Path>>(path: P) -> PathBuf {
    let mut buf = PathBuf::new();

    for comp in path.as_ref().components() {
        match comp {
            std::path::Component::Normal(name) => buf.push(name),
            std::path::Component::ParentDir => {
                buf.pop();
            }
            _ => (),
        }
    }

    buf
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadToken(String);

impl UploadToken {
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for UploadToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl<'de> serde::Deserialize<'de> for UploadToken {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let token = String::deserialize(deserializer)?;

        if token.len() != 32 {
            return Err(serde::de::Error::custom("Invalid token length"));
        }

        Ok(Self(token))
    }
}

pub struct UploadTokenEntry {
    token: UploadToken,
}

struct Controller {
    upload_tokens: Vec<UploadTokenEntry>,
}

impl Controller {
    fn new() -> Self {
        Self {
            upload_tokens: Vec::new(),
        }
    }
}

type SharedController = Arc<tokio::sync::Mutex<Controller>>;

#[derive(Clone)]
pub struct Admin {
    controller: SharedController,
}

impl Admin {
    pub async fn new_upload_token(&self) -> UploadToken {
        use rand::Rng;

        let mut controller = self.controller.lock().await;

        let mut rng = assert_crypto_secure(rand::thread_rng());

        let token = loop {
            let new_token = UploadToken(format!(
                "{:016X}{:016X}",
                rng.gen::<u64>(),
                rng.gen::<u64>()
            ));

            if !controller
                .upload_tokens
                .iter()
                .any(|current_token| current_token.token == new_token)
            {
                break new_token;
            }
        };

        controller.upload_tokens.push(UploadTokenEntry {
            token: token.clone(),
        });

        token
    }
}

#[derive(Debug)]
pub enum UploadError {
    InvalidToken,
    MissingFilename,
    InternalServerError,
}

impl UploadError {
    pub fn status_code(&self) -> StatusCode {
        match self {
            Self::InvalidToken => StatusCode::FORBIDDEN,
            Self::MissingFilename => StatusCode::BAD_REQUEST,
            Self::InternalServerError => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn internal_error<T: std::fmt::Display>(message: &'static str) -> impl FnOnce(T) -> Self {
        move |err| {
            tracing::error!("{message}: {err}");
            Self::InternalServerError
        }
    }
}

#[derive(Clone)]
pub struct User {
    controller: SharedController,
}

impl User {
    pub async fn upload_files(
        &self,
        token: UploadToken,
        mut files: Multipart,
    ) -> Result<(), UploadError> {
        let mut controller = self.controller.lock().await;

        let token = controller
            .upload_tokens
            .iter()
            .position(|current_token| current_token.token == token)
            .map(|index| controller.upload_tokens.remove(index))
            .ok_or(UploadError::InvalidToken)?;

        let upload_dir = Path::new("uploads").join(token.token.as_str());

        std::fs::create_dir(&upload_dir).map_err(UploadError::internal_error(
            "Failed to create upload directory",
        ))?;

        while let Some(mut field) = files
            .next_field()
            .await
            .map_err(UploadError::internal_error("Failed to get next file"))?
        {
            let file_name = String::from(field.file_name().ok_or_else(|| {
                tracing::error!("Missing filename");
                UploadError::MissingFilename
            })?);

            let file_path = upload_dir.join(sanitize_path(&file_name));

            tracing::info!("Uploading to {}", file_path.display());

            let mut file = std::fs::File::create(&file_path)
                .map_err(UploadError::internal_error("Failed to create file"))?;

            while let Some(blob) = field.next().await {
                let blob = blob.map_err(UploadError::internal_error("Failed to stream data"))?;

                tracing::debug!("Blob - {} bytes", blob.len());
                file.write_all(&blob)
                    .map_err(UploadError::internal_error("Failed to write file data"))?
            }

            file.flush()
                .map_err(UploadError::internal_error("Failed to close file"))?;

            tracing::debug!("Finished uploading to {}", file_path.display());
        }

        Ok(())
    }
}

pub fn new_controller() -> (Admin, User) {
    let controller = Arc::new(tokio::sync::Mutex::new(Controller::new()));

    (
        Admin {
            controller: controller.clone(),
        },
        User { controller },
    )
}
