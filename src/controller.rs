use std::{
    fmt,
    marker::PhantomData,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result};
use axum::extract::Multipart;
use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;

use crate::AppConfig;

const FILES_DIRECTORY: &str = "files";
const TOKEN_FILENAME: &str = "token.toml";

pub type Timestamp = chrono::DateTime<chrono::Local>;

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

fn create_directory<P: AsRef<Path>>(path: P) -> Result<P> {
    std::fs::create_dir(path.as_ref())
        .with_context(|| format!("Failed to create directory {}", path.as_ref().display()))?;

    Ok(path)
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
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

    pub fn as_str(&self) -> &str {
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

impl fmt::Display for ByteCount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
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

    async fn from_multipart<P: AsRef<Path>>(
        storage_directory: P,
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

            let file_path = storage_directory.as_ref().join(sanitize_path(&file_name));

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
pub struct ShareConfig {
    pub expiry: Timestamp,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UploadConfig {
    pub name: String,
    pub expiry: Timestamp,
    pub space_quota: ByteCount,
}

struct TokenConfigMutexCore;

impl TokenConfigMutexCore {
    fn load_config<C: serde::de::DeserializeOwned>(path: &Path) -> Result<C> {
        let file_contents = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        toml::from_str::<C>(&file_contents)
            .with_context(|| format!("Failed to parse {}", file_contents))
    }

    fn save_config<C: serde::Serialize>(path: &Path, config: &C) -> Result<()> {
        std::fs::write(
            path,
            toml::to_string(config).context("Failed to serialize config")?,
        )
        .with_context(|| format!("Failed to write config to {}", path.display()))
    }

    fn create_token_config<C: serde::Serialize>(&mut self, path: &Path, config: &C) -> Result<()> {
        Self::save_config(path, config)
    }

    fn token_config<C: serde::de::DeserializeOwned>(&mut self, path: &Path) -> Result<C> {
        Self::load_config(path)
    }

    fn with_token_config_mut<
        C: serde::Serialize + serde::de::DeserializeOwned,
        T,
        F: FnOnce(&mut C) -> Result<T>,
    >(
        &mut self,
        path: &Path,
        f: F,
    ) -> Result<T> {
        let mut config = Self::load_config(path)?;

        let result = f(&mut config)?;

        Self::save_config(path, &config)?;

        Ok(result)
    }
}

type TokenConfigMutex = tokio::sync::Mutex<TokenConfigMutexCore>;

struct TokenConfig<'a, C, P: AsRef<Path>> {
    path: P,
    token_config_mutex: &'a TokenConfigMutex,
    _config: PhantomData<C>,
}

impl<'a, C: serde::Serialize + serde::de::DeserializeOwned, P: AsRef<Path>> TokenConfig<'a, C, P> {
    fn new(path: P, token_config_mutex: &'a TokenConfigMutex) -> Self {
        Self {
            path,
            token_config_mutex,
            _config: PhantomData,
        }
    }

    async fn create_token_config(&self, config: &C) -> Result<()> {
        self.token_config_mutex
            .lock()
            .await
            .create_token_config(self.path.as_ref(), config)
    }

    async fn token_config(&self) -> Result<C> {
        self.token_config_mutex
            .lock()
            .await
            .token_config(self.path.as_ref())
    }

    async fn with_token_config_mut<T, F: FnOnce(&mut C) -> Result<T>>(&self, f: F) -> Result<T> {
        self.token_config_mutex
            .lock()
            .await
            .with_token_config_mut(self.path.as_ref(), f)
    }
}

struct Controller {
    config: AppConfig,
    token_config_mutex: TokenConfigMutex,
}

pub struct Filename(std::path::PathBuf);

impl Filename {
    pub fn display(&self) -> std::path::Display {
        self.0.display()
    }
}

impl<'de> serde::Deserialize<'de> for Filename {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let path = String::deserialize(deserializer)?;
        let path = path.trim_start_matches('/');
        let path = std::path::Path::new(path);

        let mut components = path.components();

        let filename = components
            .next()
            .ok_or_else(|| serde::de::Error::custom("Bad Path"))?;

        let filename = if let std::path::Component::Normal(filename) = filename {
            filename
        } else {
            return Err(serde::de::Error::custom("Bad Path"));
        };

        if components.next().is_some() {
            return Err(serde::de::Error::custom("Bad Path"));
        }

        Ok(Self(std::path::PathBuf::from(filename)))
    }
}

impl AsRef<Path> for Filename {
    fn as_ref(&self) -> &Path {
        self.0.as_path()
    }
}

impl fmt::Display for Filename {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.display().fmt(f)
    }
}

pub struct UploadListing {
    pub name: String,
    pub token: Token,
}

#[derive(askama::Template)]
#[template(path = "user_share_directory_listing.html")]
struct ShareDirectoryListing {
    files: Vec<String>,
}

#[derive(Clone)]
pub struct Admin {
    controller: Arc<Controller>,
}

impl Admin {
    pub fn config(&self) -> &AppConfig {
        &self.controller.config
    }

    pub async fn new_share_token(&self, config: ShareConfig) -> Result<Token> {
        let token = Token::new();

        let shares_directory =
            create_directory(self.config().shares_directory().join(token.as_str()))?;

        create_directory(shares_directory.join(FILES_DIRECTORY))?;
        TokenConfig::new(
            shares_directory.join(TOKEN_FILENAME),
            &self.controller.token_config_mutex,
        )
        .create_token_config(&config)
        .await?;

        Ok(token)
    }

    pub async fn share_files(&self, token: Token, files: Multipart) -> Result<()> {
        let shares_directory = self.config().shares_directory().join(token.as_str());

        let token_config = TokenConfig::<ShareConfig, _>::new(
            shares_directory.join(TOKEN_FILENAME),
            &self.controller.token_config_mutex,
        )
        .token_config()
        .await?;

        if chrono::Local::now() > token_config.expiry {
            anyhow::bail!("Token has expired");
        }

        let mut actual_file_size = ByteCount(0);

        NewFile::from_multipart(
            shares_directory.join(FILES_DIRECTORY),
            files,
            &mut actual_file_size,
        )
        .await
    }

    pub async fn current_uploads(&self) -> Result<Vec<UploadListing>> {
        let uploads_directory = self.config().uploads_directory();

        let mut upload_listings = Vec::new();

        for entry in std::fs::read_dir(&uploads_directory)
            .with_context(|| format!("Failed to read {}", uploads_directory.display()))?
        {
            let entry = entry.with_context(|| {
                format!("Failed to read entry in {}", uploads_directory.display())
            })?;
            let name = TokenConfig::<UploadConfig, _>::new(
                entry.path().join(TOKEN_FILENAME),
                &self.controller.token_config_mutex,
            )
            .token_config()
            .await?
            .name;
            let token = Token(entry.file_name().to_string_lossy().into_owned());

            upload_listings.push(UploadListing { name, token });
        }

        upload_listings.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(upload_listings)
    }

    pub async fn new_upload_token(&self, config: UploadConfig) -> Result<Token> {
        let token = Token::new();

        let name = if config.name.is_empty() {
            token.0.clone()
        } else {
            config.name
        };

        let config = UploadConfig { name, ..config };

        let token_directory =
            create_directory(self.config().uploads_directory().join(token.as_str()))?;

        create_directory(token_directory.join(FILES_DIRECTORY))?;
        TokenConfig::new(
            token_directory.join(TOKEN_FILENAME),
            &self.controller.token_config_mutex,
        )
        .create_token_config(&config)
        .await?;

        Ok(token)
    }

    pub async fn current_upload_config(&self, token: &Token) -> Result<UploadConfig> {
        TokenConfig::new(
            self.config()
                .uploads_directory()
                .join(token.as_str())
                .join(TOKEN_FILENAME),
            &self.controller.token_config_mutex,
        )
        .token_config()
        .await
    }
}

#[derive(Clone)]
pub struct User {
    controller: Arc<Controller>,
}

impl User {
    pub fn config(&self) -> &AppConfig {
        &self.controller.config
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

        let token_directory = self.config().uploads_directory().join(token.as_str());

        let token_config = TokenConfig::new(
            token_directory.join(TOKEN_FILENAME),
            &self.controller.token_config_mutex,
        );

        token_config
            .with_token_config_mut(|token_config: &mut UploadConfig| {
                if chrono::Local::now() > token_config.expiry {
                    anyhow::bail!("Token has expired");
                }

                token_config.space_quota = token_config
                    .space_quota
                    .checked_sub(request_size)
                    .context("Out of Space")?;

                Ok(())
            })
            .await?;

        let mut actual_file_size = ByteCount(0);

        let write_result = NewFile::from_multipart(
            token_directory.join(FILES_DIRECTORY),
            files,
            &mut actual_file_size,
        )
        .await;

        token_config
            .with_token_config_mut(|token_config| {
                token_config.space_quota += request_size.saturating_sub(actual_file_size);
                Ok(())
            })
            .await?;

        write_result
    }

    pub async fn directory_listing(&self, token: Token) -> Result<String> {
        use askama::Template;

        let path = self
            .config()
            .shares_directory()
            .join(token.as_str())
            .join(FILES_DIRECTORY);

        let files = std::fs::read_dir(&path)
            .with_context(|| format!("Failed to read directory {}", path.display()))?
            .map(|entry| {
                let entry =
                    entry.with_context(|| format!("Failed to read entry in {}", path.display()))?;

                Ok(entry.file_name().to_string_lossy().into_owned())
            })
            .collect::<Result<Vec<_>>>()?;

        ShareDirectoryListing { files }
            .render()
            .with_context(|| format!("Failed to render template for {}", path.display()))
    }

    pub async fn open_shared_file(
        &self,
        token: Token,
        filename: Filename,
    ) -> Result<(tokio::fs::File, std::fs::Metadata, mime_guess::Mime)> {
        let path = self
            .config()
            .shares_directory()
            .join(token.as_str())
            .join(FILES_DIRECTORY)
            .join(filename);

        let file = tokio::fs::File::open(&path)
            .await
            .with_context(|| format!("Failed to open {}", path.display()))?;

        let metadata = file
            .metadata()
            .await
            .with_context(|| format!("Failed to get metadata for {}", path.display()))?;

        let mime = mime_guess::from_path(path).first_or_octet_stream();

        Ok((file, metadata, mime))
    }
}

pub fn new_controller(config: AppConfig) -> (Admin, User) {
    let controller = Arc::new(Controller {
        config,
        token_config_mutex: TokenConfigMutex::new(TokenConfigMutexCore),
    });

    (
        Admin {
            controller: controller.clone(),
        },
        User { controller },
    )
}
