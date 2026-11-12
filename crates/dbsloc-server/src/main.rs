// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! `dbsloc_server` — the Wyss DBS Localizer GPU server binary (`docs/SERVER_CLIENT_PLAN.md` WP2 + WP3).

use std::net::SocketAddr;
use std::time::Duration;

use dbsloc_pipeline::reg_backend;
use dbsloc_server::{build_router, AppState, Config};

/// How long a graceful shutdown waits for running jobs to stop their runners.
const JOB_STOP_WAIT: Duration = Duration::from_secs(20);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::load_or_default()?;
    println!(
        "dbsloc_server: data_root={} bind={} gpu_jobs={} auth_mode={}",
        config.data_root.display(),
        config.bind,
        config.gpu_jobs,
        config.auth_mode
    );
    if let Some(dir) = config.web_dir.as_ref() {
        println!(
            "dbsloc_server: serving the web client from {}",
            dir.display()
        );
    }

    let probe = reg_backend::cached_probe();
    if !probe.available {
        eprintln!(
            "dbsloc_server: WARNING: would refuse to serve — {}; continuing (deviation from \
             docs/SERVER_CLIENT_PLAN.md §8 so plan-only / no-GPU deployments work)",
            probe.status_line()
        );
    } else {
        println!("dbsloc_server: {}", probe.status_line());
    }

    let state = AppState::new(config.clone())?;
    let app = build_router(state.clone());

    let listener = tokio::net::TcpListener::bind(&config.bind).await?;
    println!("dbsloc_server: listening on http://{}", config.bind);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal(state.clone()))
    .await?;

    state.shutdown();
    if !state.wait_for_jobs(JOB_STOP_WAIT).await {
        eprintln!("dbsloc_server: a job did not stop within {JOB_STOP_WAIT:?}; exiting anyway");
    }
    Ok(())
}

/// Wait for Ctrl-C (or SIGTERM on unix), then end the event streams and cancel running jobs so the
/// graceful shutdown does not wait on attached clients.
async fn shutdown_signal(state: AppState) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    println!("\ndbsloc_server: shutting down");
    state.shutdown();
}
