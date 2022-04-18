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

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(chrono::NaiveDateTime);

impl Timestamp {
    pub fn now() -> Self {
        Self(chrono::Local::now().naive_local())
    }

    const FORMAT: &'static str = "%FT%H:%M";

    fn format_filename(&self) -> impl fmt::Display {
        self.0.format("%Y%m%dT%H%M%S")
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.format(Self::FORMAT).fmt(f)
    }
}

impl serde::Serialize for Timestamp {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_string().serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for Timestamp {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        chrono::NaiveDateTime::parse_from_str(&String::deserialize(deserializer)?, Self::FORMAT)
            .map(Self)
            .map_err(serde::de::Error::custom)
    }
}

impl std::ops::Add<chrono::Duration> for Timestamp {
    type Output = Self;

    fn add(self, rhs: chrono::Duration) -> Self::Output {
        Self(self.0 + rhs)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Token(String);

impl Token {
    fn new() -> Self {
        use rand::Rng;

        let mut rng = assert_crypto_secure(rand::thread_rng());

        Self(format!(
            "{}_{:016X}{:016X}",
            Timestamp::now().format_filename(),
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

    async fn from_multipart(
        storage_directory: PathBuf,
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

            let file_path = storage_directory.join(sanitize_path(&file_name));

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

trait IsTokenConfig: serde::Serialize + serde::de::DeserializeOwned {
    fn storage_directory(config: &AppConfig) -> PathBuf;
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct ShareConfig {
    pub name: String,
    pub expiry: Timestamp,
}

impl IsTokenConfig for ShareConfig {
    fn storage_directory(config: &AppConfig) -> PathBuf {
        config.shares_directory()
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct UploadConfig {
    pub name: String,
    pub expiry: Timestamp,
    pub space_quota: ByteCount,
}

impl IsTokenConfig for UploadConfig {
    fn storage_directory(config: &AppConfig) -> PathBuf {
        config.uploads_directory()
    }
}

struct TokenConfigMutexCore;

impl TokenConfigMutexCore {
    fn load_config<C: serde::de::DeserializeOwned>(token_directory: &Path) -> Result<C> {
        let path = token_directory.join(TOKEN_FILENAME);

        tracing::debug!(path = %path.display(), "Loading token config");

        let file_contents = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        toml::from_str::<C>(&file_contents)
            .with_context(|| format!("Failed to parse {}", file_contents))
    }

    fn save_config<C: serde::Serialize>(token_directory: &Path, config: &C) -> Result<()> {
        let path = token_directory.join(TOKEN_FILENAME);

        tracing::debug!(path = %path.display(), "Saving token config");

        std::fs::write(
            &path,
            toml::to_string(config).context("Failed to serialize config")?,
        )
        .with_context(|| format!("Failed to write config to {}", path.display()))
    }

    fn create_token_config<C: serde::Serialize>(
        &mut self,
        token_directory: &Path,
        config: &C,
    ) -> Result<()> {
        create_directory(token_directory)?;
        create_directory(token_directory.join(FILES_DIRECTORY))?;
        Self::save_config(token_directory, config)
    }

    fn token_config<C: serde::de::DeserializeOwned>(
        &mut self,
        token_directory: &Path,
    ) -> Result<C> {
        Self::load_config(token_directory)
    }

    fn with_token_config_mut<
        C: serde::Serialize + serde::de::DeserializeOwned,
        T,
        F: FnOnce(&mut C) -> Result<T>,
    >(
        &mut self,
        token_directory: &Path,
        f: F,
    ) -> Result<T> {
        let mut config = Self::load_config(token_directory)?;

        let result = f(&mut config)?;

        Self::save_config(token_directory, &config)?;

        Ok(result)
    }
}

type TokenConfigMutex = tokio::sync::Mutex<TokenConfigMutexCore>;

struct TokenConfig<'a, C> {
    token_directory: PathBuf,
    token_config_mutex: &'a TokenConfigMutex,
    _config: PhantomData<C>,
}

impl<'a, C: serde::Serialize + serde::de::DeserializeOwned> TokenConfig<'a, C> {
    fn new(token_directory: PathBuf, token_config_mutex: &'a TokenConfigMutex) -> Self {
        Self {
            token_directory,
            token_config_mutex,
            _config: PhantomData,
        }
    }

    fn files_directory(&self) -> PathBuf {
        self.token_directory.join(FILES_DIRECTORY)
    }

    async fn create(&self, config: &C) -> Result<()> {
        self.token_config_mutex
            .lock()
            .await
            .create_token_config(&self.token_directory, config)
    }

    async fn load(&self) -> Result<C> {
        self.token_config_mutex
            .lock()
            .await
            .token_config(&self.token_directory)
    }

    async fn update<T, F: FnOnce(&mut C) -> Result<T>>(&self, f: F) -> Result<T> {
        self.token_config_mutex
            .lock()
            .await
            .with_token_config_mut(&self.token_directory, f)
    }
}

struct Controller {
    config: AppConfig,
    token_config_mutex: TokenConfigMutex,
}

impl Controller {
    fn get_token_config<C: IsTokenConfig>(&self, token: &Token) -> TokenConfig<C> {
        TokenConfig::new(
            C::storage_directory(&self.config).join(token.as_str()),
            &self.token_config_mutex,
        )
    }

    fn get_share_config(&self, token: &Token) -> TokenConfig<ShareConfig> {
        self.get_token_config(token)
    }

    fn get_upload_config(&self, token: &Token) -> TokenConfig<UploadConfig> {
        self.get_token_config(token)
    }
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
        let path = std::path::Path::new(&path);

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

pub struct ShareListing {
    pub name: String,
    pub token: Token,
}

pub struct UploadListing {
    pub name: String,
    pub token: Token,
}

#[derive(askama::Template)]
#[template(path = "user_share_directory_listing.html")]
pub struct ShareDirectoryListing {
    name: String,
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

    pub async fn current_shares(&self) -> Result<Vec<ShareListing>> {
        let shares_directory = self.config().shares_directory();

        let mut share_listings = Vec::new();

        for entry in std::fs::read_dir(&shares_directory)
            .with_context(|| format!("Failed to read {}", shares_directory.display()))?
        {
            let entry = entry.with_context(|| {
                format!("Failed to read entry in {}", shares_directory.display())
            })?;
            let token = Token(entry.file_name().to_string_lossy().into_owned());

            let name = match self
                .controller
                .get_token_config::<ShareConfig>(&token)
                .load()
                .await
            {
                Ok(token) => token.name,
                Err(err) => {
                    tracing::warn!("{err:#}");
                    continue;
                }
            };

            share_listings.push(ShareListing { name, token });
        }

        share_listings.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(share_listings)
    }

    pub async fn new_share_token(&self, config: ShareConfig) -> Result<Token> {
        let token = Token::new();

        self.controller
            .get_share_config(&token)
            .create(&config)
            .await?;

        Ok(token)
    }

    pub async fn current_share_config(&self, token: &Token) -> Result<ShareConfig> {
        self.controller.get_share_config(token).load().await
    }

    pub async fn share_files(&self, token: Token, files: Multipart) -> Result<()> {
        let token_config = self.controller.get_share_config(&token);

        if Timestamp::now() > token_config.load().await?.expiry {
            anyhow::bail!("Token has expired");
        }

        let mut actual_file_size = ByteCount(0);

        NewFile::from_multipart(token_config.files_directory(), files, &mut actual_file_size).await
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
            let token = Token(entry.file_name().to_string_lossy().into_owned());
            let name = match self
                .controller
                .get_token_config::<UploadConfig>(&token)
                .load()
                .await
            {
                Ok(token) => token.name,
                Err(err) => {
                    tracing::warn!("{err:#}");
                    continue;
                }
            };

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

        self.controller
            .get_upload_config(&token)
            .create(&config)
            .await?;

        Ok(token)
    }

    pub async fn current_upload_config(&self, token: &Token) -> Result<UploadConfig> {
        self.controller.get_upload_config(token).load().await
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

        let token_config = self.controller.get_token_config::<UploadConfig>(&token);

        token_config
            .update(|token_config| {
                if Timestamp::now() > token_config.expiry {
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

        let write_result =
            NewFile::from_multipart(token_config.files_directory(), files, &mut actual_file_size)
                .await;

        token_config
            .update(|token_config| {
                token_config.space_quota += request_size.saturating_sub(actual_file_size);
                Ok(())
            })
            .await?;

        write_result
    }

    pub async fn directory_listing(&self, token: Token) -> Result<ShareDirectoryListing> {
        let share_config = self.controller.get_share_config(&token);

        let name = share_config.load().await?.name;

        let files_directory = share_config.files_directory();

        let files = std::fs::read_dir(&files_directory)
            .with_context(|| format!("Failed to read directory {}", files_directory.display()))?
            .map(|entry| {
                let entry = entry.with_context(|| {
                    format!("Failed to read entry in {}", files_directory.display())
                })?;

                Ok(entry.file_name().to_string_lossy().into_owned())
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(ShareDirectoryListing { name, files })
    }

    pub async fn open_shared_file(
        &self,
        token: Token,
        filename: Filename,
    ) -> Result<(tokio::fs::File, std::fs::Metadata, mime_guess::Mime)> {
        let path = self
            .controller
            .get_share_config(&token)
            .files_directory()
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
