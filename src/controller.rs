use std::{
    fmt,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use axum::extract::Multipart;
use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;

const UPLOADS_DIRECTORY: &str = "uploads";
const FILES_DIRECTORY: &str = "files";
const TOKEN_FILENAME: &str = "token.toml";

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

fn create_directory<P: AsRef<Path>>(path: P) -> Result<()> {
    std::fs::create_dir(path.as_ref())
        .with_context(|| format!("Failed to create directory {}", path.as_ref().display()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    fn new() -> Self {
        use rand::Rng;

        let mut rng = assert_crypto_secure(rand::thread_rng());

        let now = chrono::offset::Local::now().format("%Y%m%dT%H%M%S");

        Self(format!(
            "{}_{:016X}{:016X}",
            now,
            rng.gen::<u64>(),
            rng.gen::<u64>()
        ))
    }

    fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl<'de> serde::Deserialize<'de> for Token {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let token = String::deserialize(deserializer)?;

        if !token.chars().all(|c| c == '_' || c.is_ascii_alphanumeric()) {
            return Err(serde::de::Error::custom("Invalid token"));
        }

        Ok(Self(token))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ByteCount(pub usize);

impl ByteCount {
    fn checked_sub(self, rhs: ByteCount) -> Option<Self> {
        self.0.checked_sub(rhs.0).map(Self)
    }

    fn saturating_sub(self, rhs: ByteCount) -> Self {
        Self(self.0.saturating_sub(rhs.0))
    }
}

impl std::ops::AddAssign for ByteCount {
    fn add_assign(&mut self, rhs: Self) {
        self.0 += rhs.0;
    }
}

impl<'de> serde::Deserialize<'de> for ByteCount {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        usize::deserialize(deserializer).map(Self)
    }
}

impl serde::Serialize for ByteCount {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.serialize(serializer)
    }
}

struct NewFile<'a> {
    filename: &'a Path,
    file: Option<tokio::fs::File>,
    size: ByteCount,
}

impl<'a> NewFile<'a> {
    async fn new(filename: &'a Path) -> Result<NewFile<'a>> {
        let file = Some(
            tokio::fs::File::create(filename)
                .await
                .with_context(|| format!("Failed to create {}", filename.display()))?,
        );

        Ok(Self {
            filename,
            file,
            size: ByteCount(0),
        })
    }

    async fn write_all(&mut self, data: &[u8]) -> Result<()> {
        self.file
            .as_mut()
            .unwrap()
            .write_all(data)
            .await
            .with_context(|| format!("Failed to write to {}", self.filename.display()))?;

        self.size.0 += data.len();

        Ok(())
    }

    async fn close(mut self) -> Result<ByteCount> {
        self.file
            .as_mut()
            .unwrap()
            .flush()
            .await
            .with_context(|| format!("Failed to flush {}", self.filename.display()))?;

        self.file.take();

        tracing::debug!("Finished writing to {}", self.filename.display());

        Ok(self.size)
    }
}

impl<'a> Drop for NewFile<'a> {
    fn drop(&mut self) {
        if self.file.take().is_some() {
            if let Err(err) = std::fs::remove_file(self.filename) {
                tracing::error!("Failed to remove {}: {}", self.filename.display(), err);
            }
        }
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct UploadConfig {
    pub expiry: chrono::DateTime<chrono::Local>,
    pub space_quota: ByteCount,
}

#[derive(Clone)]
pub struct Admin {}

impl Admin {
    pub fn new_upload_token(&self, config: UploadConfig) -> Result<Token> {
        let token = Token::new();

        let token_directory = Path::new(UPLOADS_DIRECTORY).join(token.as_str());
        create_directory(&token_directory)?;

        let files_directory = token_directory.join(FILES_DIRECTORY);
        create_directory(&files_directory)
            .with_context(|| format!("Failed to create {}", files_directory.display()))?;

        let token_filename = token_directory.join(TOKEN_FILENAME);

        std::fs::write(
            &token_filename,
            toml::to_string(&config).context("Failed to serialize config")?,
        )
        .with_context(|| format!("Failed to write config to {}", token_filename.display()))?;

        Ok(token)
    }
}

#[derive(Clone)]
pub struct User {}

impl User {
    async fn write_files(
        upload_directory: &Path,
        mut files: Multipart,
        total_size: &mut ByteCount,
    ) -> Result<()> {
        while let Some(mut field) = files
            .next_field()
            .await
            .context("Failed to get next file")?
        {
            let file_name = match field.file_name() {
                Some(file_name) => String::from(file_name),
                None => continue,
            };

            let file_path = upload_directory.join(sanitize_path(&file_name));

            tracing::info!("Uploading to {}", file_path.display());

            let mut file = NewFile::new(&file_path).await?;

            while let Some(blob) = field.next().await {
                let blob = blob.context("Failed to read data")?;
                file.write_all(&blob).await?;
            }

            *total_size += file.close().await?;

            tracing::debug!("Finished uploading to {}", file_path.display());
        }

        Ok(())
    }

    pub async fn upload_files(
        &self,
        token: Token,
        content_length: u64,
        files: Multipart,
    ) -> Result<()> {
        let request_size = ByteCount(
            content_length
                .try_into()
                .context("File upload is too large")?,
        );

        let token_directory = Path::new(UPLOADS_DIRECTORY).join(token.as_str());

        let token_filename = token_directory.join(TOKEN_FILENAME);

        let token_config =
            std::fs::read_to_string(&token_filename).context("Failed to read token config")?;
        let mut token_config = toml::from_str::<UploadConfig>(&token_config)
            .context("Failed to parse token config")?;

        if chrono::Local::now() > token_config.expiry {
            anyhow::bail!("Token has expired");
        }

        token_config.space_quota = token_config
            .space_quota
            .checked_sub(request_size)
            .context("Out of Space")?;

        let upload_directory = token_directory.join(FILES_DIRECTORY);

        let mut actual_file_size = ByteCount(0);

        let write_result = Self::write_files(&upload_directory, files, &mut actual_file_size).await;

        token_config.space_quota += request_size.saturating_sub(actual_file_size);

        std::fs::write(
            &token_filename,
            toml::to_string(&token_config).context("Failed to format token config")?,
        )
        .context("Failed to write token context")?;

        write_result
    }
}

pub fn new_controller() -> (Admin, User) {
    (Admin {}, User {})
}
