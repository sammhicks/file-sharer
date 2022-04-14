use futures_util::FutureExt;

mod admin_app;
mod controller;
mod user_app;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    tracing_subscriber::fmt::init();

    let (shutdown_handle, shutdown_signal) = tokio::sync::oneshot::channel::<()>();
    let shutdown_signal = shutdown_signal.map(|_| ()).shared();

    let (task_active_handle, mut tasks_complete_signal) = tokio::sync::mpsc::channel::<()>(1);
    let task_active_handle = move |()| drop(task_active_handle);

    let (admin, user) = controller::new_controller();

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
}
