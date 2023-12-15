use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context};
use clap::Parser;
use serde::Deserialize;
use tokio::sync::mpsc;

/// Checkouts all submodules recursively
///
/// Pass arguments on the command line, or via environment variables.
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// the passed categories of submodules will be bumped
    /// to their upstream rather than their current revision
    ///
    /// Environment variable: TETRANE_CHECKOUT_BUMP
    #[clap(short, long)]
    bump: Vec<String>,

    /// the passed categories of submodules will be ignored
    /// and not init'ed or checkout'd
    ///
    /// Environment variable: TETRANE_CHECKOUT_IGNORE
    #[clap(short, long)]
    ignore: Vec<String>,

    /// Path to the root repository containing a Checkout.toml (default: '.')
    ///
    /// Environment variable: TETRANE_CHECKOUT_REPOSITORY_PATH
    #[clap(value_parser)]
    repository_path: Option<String>,

    /// If present, forces the checkout to occur even if there are modified files (staged or not).
    ///
    /// **Enabling this option can lead to data losses.**
    ///
    /// Environment variable: TETRANE_CHECKOUT_FORCE_CHECKOUT
    #[clap(long, parse(from_flag))]
    force_checkout: bool,

    /// If present, display debug messages
    #[clap(long, parse(from_flag))]
    debug: bool,
}

impl Args {
    fn merge_from_env(&mut self) {
        if let Ok(bump) = std::env::var("TETRANE_CHECKOUT_BUMP") {
            self.bump
                .extend(bump.split_ascii_whitespace().map(ToOwned::to_owned))
        }
        if let Ok(ignore) = std::env::var("TETRANE_CHECKOUT_IGNORE") {
            self.ignore
                .extend(ignore.split_ascii_whitespace().map(ToOwned::to_owned))
        }
        if let Ok(repository_path) = std::env::var("TETRANE_CHECKOUT_REPOSITORY_PATH") {
            self.repository_path = Some(repository_path)
        }
        if let Ok(force_checkout) = std::env::var("TETRANE_CHECKOUT_FORCE_CHECKOUT") {
            self.force_checkout =
                force_checkout != "0" && force_checkout.to_ascii_lowercase() != "false"
        }
    }
}

const CONFIG_FILE: &str = "Checkout.toml";

fn main() -> anyhow::Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("Cannot spawn tokio runtime")?
        .block_on(async_main())
}

async fn async_main() -> anyhow::Result<()> {
    let mut args = Args::parse();
    args.merge_from_env();

    let tracer = tracing_subscriber::FmtSubscriber::builder()
        .with_max_level(if args.debug {
            tracing::Level::DEBUG
        } else {
            tracing::Level::INFO
        })
        .with_target(false)
        .without_time()
        .with_line_number(false)
        .with_file(false)
        .finish();
    tracing::subscriber::set_global_default(tracer)?;

    let root_repository = PathBuf::from(args.repository_path.unwrap_or_else(|| ".".into()));
    let root_repository = root_repository
        .canonicalize()
        .with_context(|| format!("Cannot resolve '{}'", root_repository.display()))?;
    let config = Config::load(&root_repository)
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
    handle_repository(
        root_repository.clone(),
        root_repository,
        args.bump,
        args.ignore,
        error_sender,
        config,
        args.force_checkout,
    )
    .await?;
    report_errors(error_receiver).await
}

async fn report_errors(
    mut error_receiver: mpsc::UnboundedReceiver<anyhow::Result<CheckoutResult>>,
) -> anyhow::Result<()> {
    use owo_colors::OwoColorize;

    let mut changed = 0;
    let mut unchanged = 0;
    let mut errors = Vec::new();

    while let Some(result) = error_receiver.recv().await {
        match result {
            Ok(CheckoutResult::Changed) => changed += 1,
            Ok(CheckoutResult::Unchanged) => unchanged += 1,
            Err(error) => errors.push(error),
        }
    }

    if errors.is_empty() {
        println!(
            "\n{}: {} checkouts, {} unchanged",
            "SUCCESS".bright_green(),
            changed.green(),
            unchanged.dimmed(),
        );
        Ok(())
    } else {
        println!(
            "\n{}: {} checkouts, {} unchanged, {} error(s)",
            "FAIL".bright_red(),
            changed.green(),
            unchanged.dimmed(),
            errors.len().red()
        );
        println!("\nCommand failed due to the following error(s):\n");
        for error in errors {
            let mut error: &dyn std::error::Error = error.as_ref();
            println!("Error: {}", error);
            while let Some(source) = error.source() {
                println!("  Caused by: {}", source);
                error = source;
            }
        }
        bail!("Command terminated with errors")
    }
}

#[derive(Debug, Deserialize, Clone)]
struct Config {
    submodules: HashMap<PathBuf, Submodule>,
}

impl Config {
    fn from_repository(
        repository: git2::Repository,
        categories: Vec<String>,
    ) -> anyhow::Result<Self> {
        let submodules = repository
            .submodules()
            .context("Could not load submodules")?
            .into_iter()
            .map(|submodule| {
                (
                    submodule.path().to_owned(),
                    Submodule {
                        categories: categories.clone(),
                        recursive: Some(true),
                        name: submodule.name().map(ToOwned::to_owned),
                    },
                )
            })
            .collect();
        Ok(Self { submodules })
    }

    #[tracing::instrument(level = "trace")]
    fn load(path: &Path) -> anyhow::Result<Option<Self>> {
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
}

#[derive(Debug, Deserialize, Clone)]
struct Submodule {
    categories: Vec<String>,
    recursive: Option<bool>,
    name: Option<String>,
}

trait UnwrapOrResumeUnwind {
    type T;
    fn unwrap_or_resume_unwind(self) -> Self::T;
}

impl<T> UnwrapOrResumeUnwind for Result<T, tokio::task::JoinError> {
    type T = T;

    fn unwrap_or_resume_unwind(self) -> Self::T {
        match self {
            Ok(ok) => ok,
            Err(err) => match err.try_into_panic() {
                Ok(payload) => std::panic::resume_unwind(payload),
                Err(_) => panic!("Task was unexpectedly cancelled"),
            },
        }
    }
}

fn handle_repository(
    root_path: PathBuf,
    path: PathBuf,
    bumped_categories: Vec<String>,
    ignored_categories: Vec<String>,
    error_sender: mpsc::UnboundedSender<anyhow::Result<CheckoutResult>>,
    config: Config,
    force_checkout: bool,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send>> {
    Box::pin(async move {
        tracing::trace!("Handling repo {}", path.display());

        let mut tasks = Vec::new();

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
            let cloned_root_path = root_path.clone();
            let cloned_path = path.clone();
            let recursive = submodule.recursive.unwrap_or(false);
            let cloned_submodule_path = submodule_path.clone();
            let cloned_bumped_categories = bumped_categories.clone();
            let cloned_ignored_categories = ignored_categories.clone();
            let cloned_error_sender = error_sender.clone();
            tasks.push(tokio::spawn(async move {
                let checkout_result = {
                    let cloned_path = cloned_path.clone();
                    let cloned_submodule_path = cloned_submodule_path.clone();

                    tokio::task::spawn_blocking(move || {
                        let cloned_path = cloned_path.clone();
                        let cloned_submodule_path = cloned_submodule_path.clone();

                        checkout_repo(
                            cloned_path,
                            cloned_submodule_path,
                            operation,
                            force_checkout,
                            submodule.name,
                        )
                    })
                    .await
                    .unwrap_or_resume_unwind()
                };
                if let Err(error) = &checkout_result {
                    tracing::error!(
                        ?error,
                        ?cloned_path,
                        ?cloned_submodule_path,
                        "Error while checkouting repository"
                    );
                } else {
                    let submodule_path = cloned_path.join(cloned_submodule_path);
                    let config = (|| {
                        if recursive {
                            let submodule_categories = submodule.categories.clone();

                            let repository = git2::Repository::open(&submodule_path)
                                .context("Could not open submodule")?;
                            Config::from_repository(repository, submodule_categories)
                                .context("Could not load config")
                                .map(Some)
                        } else {
                            Config::load(&submodule_path).context("Error reading configuration")
                        }
                    })();

                    match config {
                        Ok(Some(config)) => {
                            if let Err(error) = handle_repository(
                                cloned_root_path.clone(),
                                submodule_path.clone(),
                                cloned_bumped_categories,
                                cloned_ignored_categories,
                                cloned_error_sender.clone(),
                                config,
                                force_checkout,
                            )
                            .await
                            {
                                tracing::error!(?error, "Error while checkouting submodule");
                                // ignore unexpectedly dropped receiver
                                let _ = cloned_error_sender.send(Err(error.context(format!(
                                    "Error while checkouting {}",
                                    submodule_path.display()
                                ))));
                            }
                        }
                        Ok(None) => {}
                        Err(error) => {
                            // ignore unexpectedly dropped receiver
                            let _ = cloned_error_sender.send(Err(error));
                        }
                    }
                }
                // ignore unexpectedly dropped receiver
                let _ = cloned_error_sender.send(checkout_result.with_context(|| {
                    format!(
                        "Error while checkouting {}",
                        cloned_path
                            .join(&submodule_path)
                            .strip_prefix(cloned_root_path)
                            .unwrap_or(&cloned_path)
                            .display(),
                    )
                }));
            }));
        }

        for task in tasks {
            task.await.unwrap_or_resume_unwind();
        }
        Ok(())
    })
}

#[derive(Debug, Clone, Copy)]
enum Operation {
    Bump,
    Checkout,
}

fn retry_if_net<T, F>(try_this: F) -> Result<T, git2::Error>
where
    F: FnMut() -> Result<T, git2::Error>,
{
    retry_if(
        try_this,
        |error| {
            matches!(error.class(), git2::ErrorClass::Net)
                || (matches!(error.class(), git2::ErrorClass::Ssh)
                    && !matches!(error.code(), git2::ErrorCode::Auth))
        },
        Some(10),
        Some(std::time::Duration::from_secs(1)),
    )
}

fn retry_if_locked<T, F>(try_this: F) -> Result<T, git2::Error>
where
    F: FnMut() -> Result<T, git2::Error>,
{
    retry_if(
        try_this,
        |error| matches!(error.code(), git2::ErrorCode::Locked),
        Some(3),
        Some(std::time::Duration::from_millis(1000)),
    )
}

fn retry_if<T, E, F, G>(
    mut try_this: F,
    mut continue_on: G,
    retry_counts: Option<usize>,
    try_interval: Option<std::time::Duration>,
) -> Result<T, E>
where
    F: FnMut() -> Result<T, E>,
    G: FnMut(&E) -> bool,
{
    match (try_this(), retry_counts) {
        (Ok(t), _) => Ok(t),
        (Err(error), None) => {
            let mut error = error;

            loop {
                if !continue_on(&error) {
                    return Err(error);
                } else {
                    if let Some(try_interval) = try_interval {
                        std::thread::sleep(try_interval)
                    }
                    match try_this() {
                        Ok(t) => return Ok(t),
                        Err(new_error) => error = new_error,
                    }
                }
            }
        }
        (Err(error), Some(retry_counts)) => {
            let mut error = error;

            for _ in 0..retry_counts {
                if !continue_on(&error) {
                    return Err(error);
                } else {
                    if let Some(try_interval) = try_interval {
                        std::thread::sleep(try_interval)
                    }
                    match try_this() {
                        Ok(t) => return Ok(t),
                        Err(new_error) => error = new_error,
                    }
                }
            }
            Err(error)
        }
    }
}

pub enum CheckoutResult {
    Changed,
    Unchanged,
}

#[tracing::instrument]
fn checkout_repo(
    path: PathBuf,
    submodule_path: PathBuf,
    operation: Operation,
    force_checkout: bool,
    submodule_name: Option<String>,
) -> anyhow::Result<CheckoutResult> {
    let repository = git2::Repository::open(&path)
        .with_context(|| format!("Could not open repository at path '{}'", path.display()))?;

    let submodule = submodule_name.as_deref().map(Ok).unwrap_or_else(|| {
        submodule_path
            .to_str()
            .with_context(|| format!("Unsupported Non-UTF8 path '{}'", submodule_path.display()))
    })?;

    let mut submodule = repository
        .find_submodule(submodule)
        .context("Could not find submodule")?;

    // TODO: "--force" mode that will pass true to overwrite parameter
    retry_if_locked(|| submodule.init(false)).context("Could not init submodule")?;

    // TODO: make optional
    retry_if_locked(|| submodule.sync()).context("Could not sync submodule")?;
    let branch = submodule.branch().map(ToOwned::to_owned);

    let head_id = submodule.head_id().context("no head id for submodule")?;
    let workdir_id = submodule.workdir_id();

    tracing::trace!(%head_id, ?workdir_id);

    // Attempt to open submodule, or creates it if it doesn't currently exist
    let submodule = match submodule.open() {
        Ok(submodule) => submodule,
        Err(_) => {
            // Init submodule
            submodule
                .repo_init(true)
                .context("Could not initialize submodule")?;

            // Read .git file so that we can restore it later if needed
            let git_file = path.join(&submodule_path).join(".git");
            let git_file_content =
                std::fs::read_to_string(&git_file).context("Not able to read .git file")?;

            // Then clone the submodule
            let mut options = git2::SubmoduleUpdateOptions::new();
            options.fetch(ssh_agent_fetch_options());
            options.allow_fetch(false);

            let clone_result = submodule.clone(Some(&mut options));

            if let Err(error) = clone_result {
                if matches!(error.class(), git2::ErrorClass::Os) {
                    // If the server isn't answering correctly, the .git file will disappear and leave us in a
                    // non recoverable state, a workaround is to restore the .git manually before updating
                    if !git_file.exists() {
                        tracing::debug!("Re-creating .git file");
                        std::fs::write(&git_file, git_file_content)
                            .context("Unable to write .git file")?;
                    }

                    // Do not re-do the clone as this will fail if the clone was partially done
                    // The update & fetch should detect any issue and will handle it correctly
                } else {
                    Err(error).context("Could not clone submodule")?;
                }
            }

            let fetch_result = retry_if_net(|| submodule.update(true, Some(&mut options)));

            let fetch_result = if let Err(error) = &fetch_result {
                // ignore if the update failed due to missing commit: we'll try fetching it again later
                if matches!(error.class(), git2::ErrorClass::Odb)
                    && matches!(error.code(), git2::ErrorCode::NotFound)
                {
                    Ok(())
                } else {
                    fetch_result
                }
            } else {
                fetch_result
            };

            fetch_result.context("Could not update submodule")?;

            // Set the origin remote
            retry_if_locked(|| submodule.sync())
                .context("Could not sync submodule")
                .unwrap();

            submodule.open().context("Could not open submodule")?
        }
    };

    let mut untracked_files = Vec::new();

    // check submodule's current status, fail if there are modified or staged files
    if !force_checkout {
        if !matches!(submodule.state(), git2::RepositoryState::Clean) {
            bail!("The submodule is not in a clean state (is a rebase in progress?")
        }
        let mut status_options = git2::StatusOptions::default();
        status_options.include_untracked(true);
        let statuses = submodule
            .statuses(Some(&mut status_options))
            .context("Could not access submodule status")?;
        if !statuses.is_empty() {
            let mut conflicts = 0;
            let mut indexed = 0;
            let mut modified = 0;
            for entry in statuses.iter() {
                let path = if let Some(path) = entry.path() {
                    path
                } else {
                    continue;
                };

                // We need to handle submodules specially, because we don't want to lose their contents if modified,
                // but at the same time we don't want to stop just because they are not at the right commit,
                // since it is kind of the point to checkout them :-).
                // This checks if the conflicting path is a submodule, and if so, it attempts to get its status.
                // If the submodule is not in the index of this repository, we can safely ignore it:
                // - the checkout operation ignores submodule.
                // - the "dirtiness" of the submodule's worktree will be detected afterwards as long as it has a
                //   Checkout.toml.
                if let Ok(status) =
                    submodule.submodule_status(path, git2::SubmoduleIgnore::Untracked)
                {
                    if !status.intersects(
                        git2::SubmoduleStatus::INDEX_ADDED
                            | git2::SubmoduleStatus::INDEX_DELETED
                            | git2::SubmoduleStatus::INDEX_MODIFIED,
                    ) {
                        continue;
                    }
                }
                if entry.status().is_conflicted() {
                    conflicts += 1;
                } else if entry.status().intersects(
                    git2::Status::INDEX_DELETED
                        | git2::Status::INDEX_MODIFIED
                        | git2::Status::INDEX_NEW
                        | git2::Status::INDEX_RENAMED
                        | git2::Status::INDEX_TYPECHANGE,
                ) {
                    indexed += 1;
                } else if entry.status().intersects(
                    git2::Status::WT_DELETED
                        | git2::Status::WT_MODIFIED
                        | git2::Status::WT_RENAMED
                        | git2::Status::WT_TYPECHANGE,
                ) {
                    modified += 1
                } else if entry.status().is_wt_new() {
                    untracked_files.push(path.to_string());
                    continue;
                };
            }
            if conflicts != 0 || indexed != 0 || modified != 0 {
                bail!(format!(
                    "Repository '{}' is not clean: {} conflicts, {} indexed, {} modified",
                    submodule_path.display(),
                    conflicts,
                    indexed,
                    modified,
                ));
            }
        }
    }

    // Determine target commit depending on operation
    let target_id = match operation {
        Operation::Checkout => head_id,
        Operation::Bump => {
            let branch = branch.unwrap_or_else(|| "HEAD".into());

            let mut remote = submodule
                .find_remote("origin")
                .context("Could not find submodule remote")?;

            retry_if_net(|| remote.fetch(&[&branch], Some(&mut ssh_agent_fetch_options()), None))
                .context("Could not fetch remote branch")?;
            let branch = format!("refs/remotes/origin/{}", branch);

            let reference = submodule.find_reference(&branch).context(format!(
                "Could not find reference {branch} in submodule {}",
                submodule_path.display()
            ))?;

            let commit = reference.peel_to_commit().context(format!(
                "Could not get commit from reference {} in submodule {}",
                branch,
                submodule_path.display()
            ))?;
            commit.id()
        }
    };

    if let Some(workdir_id) = workdir_id {
        if workdir_id == target_id {
            tracing::trace!("HEAD already at {}", target_id);
            checkout_head(&submodule)?;

            return Ok(CheckoutResult::Unchanged);
        }
    }

    // optimistically attempt to set the commit
    let commit = match submodule.find_commit(target_id) {
        Ok(commit) => commit,
        Err(_) => {
            let mut remote = submodule
                .find_remote("origin")
                .context("Could not find submodule remote")?;

            // optimistically fetches only requested commit
            let fetch_result = retry_if_net(|| {
                remote.fetch(
                    &[format!("+{}:{}", target_id, target_id)],
                    Some(&mut ssh_agent_fetch_options()),
                    None,
                )
            });

            // some servers don't allow fetching one commit; in that case fetch everything
            let fetch_result = if let Err(error) = &fetch_result {
                if matches!(error.class(), git2::ErrorClass::Odb)
                    && matches!(error.code(), git2::ErrorCode::NotFound)
                {
                    retry_if_net(|| {
                        remote.fetch::<&str>(&[], Some(&mut ssh_agent_fetch_options()), None)
                    })
                } else {
                    fetch_result
                }
            } else {
                fetch_result
            };

            // if both fetches failed, then exit
            fetch_result.context("Could not fetch submodule")?;

            submodule
                .find_commit(target_id)
                .context("Could not find commit in submodule")?
        }
    };

    // Check the edge case of files that are currently untracked, but were in the commit we're targeting.
    // We don't want these files to be accidentally overwritten by the checkout
    if !force_checkout {
        let commit_tree = commit.tree().context("Could not get commit's tree")?;

        for path in &untracked_files {
            if commit_tree.get_name(path).is_some() {
                bail!(format!("'{path}' would be overwritten by checkout"))
            }
        }
    }

    submodule
        .set_head_detached(target_id)
        .context("Could not set head in submodule")?;

    // set_head_detached tend to index stuff, clear everything (nothing was indexed before as per the status check)
    let mut index = submodule
        .index()
        .context("Could not get submodule's index file")?;
    let head_tree = submodule.head()?.peel_to_tree()?;
    index.read_tree(&head_tree)?;
    index.write()?;

    if let Some(workdir_id) = workdir_id {
        tracing::info!("{} -> {}", workdir_id, target_id);
    } else {
        tracing::info!("initialized at {}", target_id);
    }

    checkout_head(&submodule)?;

    Ok(CheckoutResult::Changed)
}

fn checkout_head(repository: &git2::Repository) -> anyhow::Result<()> {
    let mut cb = git2::build::CheckoutBuilder::new();
    cb.force();

    repository
        .checkout_head(Some(&mut cb))
        .context("Could not checkout head")?;

    Ok(())
}

fn ssh_agent_fetch_options() -> git2::FetchOptions<'static> {
    let mut cbs = git2::RemoteCallbacks::new();
    cbs.credentials(|_url, username_from_url, _allowed_types| {
        if let Some(username_from_url) = username_from_url {
            git2::Cred::ssh_key_from_agent(username_from_url)
        } else {
            Err(git2::Error::from_str(&format!(
                "Unsupported authentication mode for {}",
                _url
            )))
        }
    });
    cbs.certificate_check(|_cert, _s| {
        tracing::warn!(%_s, "Ignoring certificate");
        Ok(git2::CertificateCheckStatus::CertificateOk)
    });
    let mut fetch_opts = git2::FetchOptions::new();
    fetch_opts.remote_callbacks(cbs);
    fetch_opts
}

#[cfg(test)]
mod test {
    #[test]
    fn libgit_correctly_compiled() {
        let version = git2::Version::get();
        assert!(version.https());
        assert!(version.ssh());
        assert!(version.threads());
    }
}
