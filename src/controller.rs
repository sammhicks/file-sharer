use std::{
    fmt,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use axum::extract::Multipart;
use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;

const SHARES_DIRECTORY: &str = "shares";
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

fn create_directory<P: AsRef<Path>>(path: P) -> Result<P> {
    std::fs::create_dir(path.as_ref())
        .with_context(|| format!("Failed to create directory {}", path.as_ref().display()))?;

    Ok(path)
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
    pub expiry: chrono::DateTime<chrono::Local>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct UploadConfig {
    pub expiry: chrono::DateTime<chrono::Local>,
    pub space_quota: ByteCount,
}

struct TokenConfig<C, P> {
    inner: C,
    path: P,
}

impl<C, P> TokenConfig<C, P> {
    fn new(path: P, config: C) -> Self {
        Self {
            inner: config,
            path,
        }
    }
}

impl<C: std::fmt::Debug + serde::Serialize + serde::de::DeserializeOwned, P: AsRef<Path>>
    TokenConfig<C, P>
{
    fn load(path: P) -> Result<Self> {
        let file_contents = std::fs::read_to_string(path.as_ref())
            .with_context(|| format!("Failed to read {}", path.as_ref().display()))?;
        let inner = toml::from_str::<C>(&file_contents)
            .with_context(|| format!("Failed to parse {}", file_contents))?;

        Ok(Self { inner, path })
    }

    fn store(self) -> Result<()> {
        std::fs::write(
            self.path.as_ref(),
            toml::to_string(&self.inner).context("Failed to serialize config")?,
        )
        .with_context(|| format!("Failed to write config to {}", self.path.as_ref().display()))
    }
}

impl<C, P> std::ops::Deref for TokenConfig<C, P> {
    type Target = C;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<C, P> std::ops::DerefMut for TokenConfig<C, P> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
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

        tracing::info!(%path);

        let path = path.trim_start_matches('/');
        let path = std::path::Path::new(path);

        for component in path.components() {
            tracing::info!(?component);
        }

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

#[derive(askama::Template)]
#[template(path = "user_share_directory_listing.html")]
struct ShareDirectoryListing {
    files: Vec<String>,
}

#[derive(Clone)]
pub struct Admin {}

impl Admin {
    pub fn new_share_token(&self, config: ShareConfig) -> Result<Token> {
        let token = Token::new();

        let token_directory = create_directory(Path::new(SHARES_DIRECTORY).join(token.as_str()))?;

        create_directory(token_directory.join(FILES_DIRECTORY))?;
        TokenConfig::new(token_directory.join(TOKEN_FILENAME), config).store()?;

        Ok(token)
    }

    pub async fn share_files(&self, token: Token, files: Multipart) -> Result<()> {
        let token_directory = Path::new(SHARES_DIRECTORY).join(token.as_str());

        let token_config =
            TokenConfig::<ShareConfig, _>::load(token_directory.join(TOKEN_FILENAME))?;

        if chrono::Local::now() > token_config.expiry {
            anyhow::bail!("Token has expired");
        }

        let mut actual_file_size = ByteCount(0);

        NewFile::from_multipart(
            token_directory.join(FILES_DIRECTORY),
            files,
            &mut actual_file_size,
        )
        .await
    }

    pub fn new_upload_token(&self, config: UploadConfig) -> Result<Token> {
        let token = Token::new();

        let token_directory = create_directory(Path::new(UPLOADS_DIRECTORY).join(token.as_str()))?;

        create_directory(token_directory.join(FILES_DIRECTORY))?;
        TokenConfig::new(token_directory.join(TOKEN_FILENAME), config).store()?;

        Ok(token)
    }
}

#[derive(Clone)]
pub struct User {}

impl User {
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

        let mut token_config =
            TokenConfig::<UploadConfig, _>::load(token_directory.join(TOKEN_FILENAME))?;

        if chrono::Local::now() > token_config.expiry {
            anyhow::bail!("Token has expired");
        }

        token_config.space_quota = token_config
            .space_quota
            .checked_sub(request_size)
            .context("Out of Space")?;

        let mut actual_file_size = ByteCount(0);

        let write_result = NewFile::from_multipart(
            token_directory.join(FILES_DIRECTORY),
            files,
            &mut actual_file_size,
        )
        .await;

        token_config.space_quota += request_size.saturating_sub(actual_file_size);

        token_config.store()?;

        write_result
    }

    pub async fn directory_listing(&self, token: Token) -> Result<String> {
        use askama::Template;

        let path = Path::new(SHARES_DIRECTORY)
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
        let path = Path::new(SHARES_DIRECTORY)
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

pub fn new_controller() -> (Admin, User) {
    (Admin {}, User {})
}
