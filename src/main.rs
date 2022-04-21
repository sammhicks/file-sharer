use std::path::PathBuf;

use clap::StructOpt;
use futures_util::FutureExt;

mod admin_app;
mod controller;
mod user_app;

fn parse_user_root(path: &str) -> String {
    format!("/{}", path.trim_matches('/'))
}

#[derive(Debug, clap::Parser)]
#[clap(name = "File Sharer")]
/// Easily share and upload files, protected by access tokens
pub struct AppConfig {
    #[clap(long, default_value = ".")]
    /// Where to store files
    files: PathBuf,
    #[clap(long, default_value = "shares")]
    /// Where to store shares (relative to files)
    shares: PathBuf,
    #[clap(long, default_value = "uploads")]
    /// Where to store uploads (relative to files)
    uploads: PathBuf,

    #[clap(long)]
    /// Disable the admin app
    disable_admin_app: bool,

    #[clap(long, default_value = "8000")]
    /// The port to listen on for the admin app
    admin_port: u16,

    #[clap(long)]
    /// Redirect to the user app using https
    user_https: bool,

    #[clap(long, default_value = "localhost:8080")]
    /// The domain hosting the user app, possibly including the port if not default
    user_host: String,

    #[clap(long, short = 'p', default_value = "8080")]
    /// The port to listen on for the user app.
    user_port: u16,

    #[clap(long, default_value = "/", parse(from_str = parse_user_root))]
    /// The root path of the user app
    user_root: String,

    #[clap(long)]
    /// Bind the user app to localhost only (useful for dev)
    user_localhost_only: bool,
}

impl AppConfig {
    fn shares_directory(&self) -> PathBuf {
        self.files.join(&self.shares)
    }

    fn uploads_directory(&self) -> PathBuf {
        self.files.join(&self.uploads)
    }

    fn token_url(&self, category: &str, token: &controller::Token) -> String {
        let scheme = if self.user_https { "https" } else { "http" };

        let domain = &self.user_host;

        let path = Some(self.user_root.as_str())
            .filter(|&path| path != "/")
            .unwrap_or("");

        format!("{scheme}://{domain}{path}/{category}/{token}")
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let config = AppConfig::try_parse()?;

    tracing::info!(?config);

    let (shutdown_handle, shutdown_signal) = tokio::sync::oneshot::channel::<()>();
    let shutdown_signal = shutdown_signal.map(|_| ()).shared();

    let (task_active_handle, mut tasks_complete_signal) = tokio::sync::mpsc::channel::<()>(1);
    let task_active_handle = move |()| drop(task_active_handle);

    let (admin, user) = controller::new_controller(config);

    let admin_app = tokio::spawn(
        admin_app::run(admin, shutdown_signal.clone()).map(task_active_handle.clone()),
    );
    let user_app = tokio::spawn(user_app::run(user, shutdown_signal).map(task_active_handle));

    let interrupt = tokio::spawn(async {
        tokio::signal::ctrl_c().await.unwrap();
        tracing::info!("Shutdown signal received");
    });

    futures_util::future::select_ok([admin_app, user_app, interrupt])
        .await
        .ok();

    drop(shutdown_handle);

    tasks_complete_signal.recv().await;

    Ok(())
}
