use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context};
use clap::Parser;
use serde::Deserialize;
use tokio::sync::mpsc;

/// Checkouts all submodules recursively
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// the passed categories of submodules will be bumped
    /// to their upstream rather than their current revision
    #[clap(short, long)]
    bump: Vec<String>,

    /// the passed categories of submodules will be ignored
    /// and not init'ed or checkout'd
    #[clap(short, long)]
    ignore: Vec<String>,

    /// Path to the root repository containing a Checkout.toml (default: '.')
    #[clap(value_parser)]
    repository_path: Option<String>,
}

const CONFIG_FILE: &str = "Checkout.toml";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let tracer = tracing_subscriber::FmtSubscriber::builder()
        .with_max_level(tracing::Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(tracer)?;
    let args = Args::parse();
    let root_repository = PathBuf::from(args.repository_path.unwrap_or_else(|| ".".into()));
    let root_repository = root_repository
        .canonicalize()
        .with_context(|| format!("Cannot resolve '{}'", root_repository.display()))?;
    load_config(&root_repository)
        .context(format!(
            "Error reading configuration in '{}'",
            root_repository.display()
        ))?
        .with_context(|| {
            format!(
                "Could not find 'Checkout.toml' in '{}'",
                root_repository.display()
            )
        })?;
    let (error_sender, error_receiver) = mpsc::unbounded_channel();
    handle_repository(root_repository, args.bump, args.ignore, error_sender).await?;
    report_errors(error_receiver).await
}

async fn report_errors(
    mut error_receiver: mpsc::UnboundedReceiver<anyhow::Result<()>>,
) -> anyhow::Result<()> {
    let mut success = 0;
    let mut errors = Vec::new();

    while let Some(result) = error_receiver.recv().await {
        match result {
            Ok(_) => success += 1,
            Err(error) => errors.push(error),
        }
    }

    if errors.is_empty() {
        println!("\nSUCCESS: {} checkouts", success);
        Ok(())
    } else {
        println!("\nFAIL: {} checkouts, {} errors", success, errors.len());
        println!("Reproducing errors here for your convenience\n");
        for error in errors {
            let mut error: &dyn std::error::Error = error.as_ref();
            println!("Error: {}", error);
            while let Some(source) = error.source() {
                println!("Caused by: {}", source);
                error = source;
            }
        }
        bail!("Command terminated with errors")
    }
}

#[derive(Debug, Deserialize)]
struct Config {
    submodules: HashMap<PathBuf, Submodule>,
}

#[derive(Debug, Deserialize)]
struct Submodule {
    categories: Vec<String>,
    recursive: Option<bool>,
}

#[tracing::instrument(level = "trace")]
fn load_config(path: &Path) -> anyhow::Result<Option<Config>> {
    let config_path: PathBuf = [path, Path::new(CONFIG_FILE)].iter().collect();
    let config = std::fs::read_to_string(&config_path);
    match config {
        Ok(config) => {
            let config = toml::from_str(&config).with_context(|| {
                format!(
                    "Could not deserialize '{}' as a configuration object",
                    config_path.display()
                )
            })?;
            Ok(Some(config))
        }
        Err(error) => match error.kind() {
            std::io::ErrorKind::NotFound => Ok(None),
            _ => bail!("Could not open '{}' as a toml file", config_path.display()),
        },
    }
}

fn handle_repository<'a>(
    path: PathBuf,
    bumped_categories: Vec<String>,
    ignored_categories: Vec<String>,
    error_sender: mpsc::UnboundedSender<anyhow::Result<()>>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + 'a + Send>> {
    Box::pin(async move {
        tracing::trace!("Handling repo {}", path.display());
        let config = load_config(&path).context("Error reading configuration")?;
        let mut tasks = Vec::new();
        let config = if let Some(config) = config {
            config
        } else {
            let component = if let Ok(component) =
                path.strip_prefix("/home/tetrane/Documents/snoopy/git/octopus_checkout_rs/")
            {
                tracing::trace!("component {}", component.display());
                component
            } else {
                tracing::trace!("prefix not found in path {}", path.display());
                return Ok(());
            };
            let octopus_path = PathBuf::from("/home/tetrane/dev/octopus").join(component);
            tracing::trace!("checking octopus_path {}", octopus_path.display());
            let config = load_config(&octopus_path).context("Error reading configuration")?;
            if let Some(config) = config {
                tracing::trace!("Using config from octopus at '{}'", octopus_path.display());
                config
            } else {
                return Ok(());
            }
        };
        tracing::trace!("Repo {} has a configuration", path.display());

        'submodules: for (submodule_path, submodule) in config.submodules {
            let mut operation = Operation::Checkout;
            for category in &submodule.categories {
                if ignored_categories.contains(category) {
                    continue 'submodules;
                }
                if bumped_categories.contains(category) {
                    operation = Operation::Bump;
                }
            }
            let cloned_path = path.clone();
            let recursive = submodule.recursive.unwrap_or(false);
            let cloned_submodule_path = submodule_path.clone();
            let cloned_bumped_categories = bumped_categories.clone();
            let cloned_ignored_categories = ignored_categories.clone();
            let cloned_error_sender = error_sender.clone();
            tasks.push(tokio::spawn(async move {
                let checkout_result =
                    checkout(&cloned_path, &cloned_submodule_path, operation, recursive).await;
                if let Err(error) = &checkout_result {
                    tracing::error!(?error)
                } else if let Err(error) = handle_repository(
                    cloned_path.join(cloned_submodule_path),
                    cloned_bumped_categories,
                    cloned_ignored_categories,
                    cloned_error_sender.clone(),
                )
                .await
                {
                    tracing::error!(?error);
                    // ignore unexpectedly dropped receiver
                    let _ = cloned_error_sender.send(Err(error));
                }
                // ignore unexpectedly dropped receiver
                let _ = cloned_error_sender.send(checkout_result);
            }));
        }

        for task in tasks {
            task.await.expect("Panicked in task");
        }
        Ok(())
    })
}

#[derive(Debug, Clone, Copy)]
enum Operation {
    Bump,
    Checkout,
}

#[tracing::instrument]
async fn checkout(
    path: &Path,
    submodule_path: &Path,
    operation: Operation,
    recursive: bool,
) -> anyhow::Result<()> {
    loop {
        let mut update_cmd = tokio::process::Command::new("git");
        update_cmd.args(["submodule", "update", "--init"]);
        if matches!(operation, Operation::Bump) {
            update_cmd.arg("--remote");
        }
        if recursive {
            update_cmd.args(["--recursive"]);
        }
        update_cmd.arg("--").arg(&submodule_path).current_dir(&path);
        let output = update_cmd.output().await.with_context(|| {
            format!(
                "Could not update submodule '{}' in '{}'",
                submodule_path.display(),
                path.display(),
            )
        })?;
        if output.status.success() {
            tracing::info!("Done checkouting {}", submodule_path.display());

            return Ok(());
        } else {
            let error_code = output
                .status
                .code()
                .map(|x| x.to_string())
                .unwrap_or_else(|| "?".into());
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("error: could not lock config file") {
                tracing::warn!("git repository locked");
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                continue;
            }
            tracing::error!(%error_code, %stderr, "could not update submodule");
            bail!(
                "Could not update submodule: error code {}\n\tError message: {}",
                error_code,
                stderr
            )
        }
    }
}
