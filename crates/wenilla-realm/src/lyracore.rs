use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

#[derive(Clone)]
pub struct Provisioner {
    executable: PathBuf,
    environment: BTreeMap<String, String>,
    operation: std::sync::Arc<tokio::sync::Mutex<()>>,
}

impl std::fmt::Debug for Provisioner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Provisioner")
            .field("executable", &self.executable)
            .finish_non_exhaustive()
    }
}

impl Provisioner {
    pub fn new(executable: PathBuf, environment: BTreeMap<String, String>) -> Self {
        Self {
            executable,
            environment,
            operation: Default::default(),
        }
    }

    pub(crate) async fn lock(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.operation.lock().await
    }

    pub fn from_env() -> Result<Self> {
        let executable: PathBuf = required("LYRACORE_GATEWAY_BIN")?.into();
        if !executable.is_absolute() || !executable.is_file() {
            bail!("LYRACORE_GATEWAY_BIN must name an existing absolute executable path");
        }
        let mut environment = BTreeMap::new();
        for key in [
            "LYRACORE_SPACETIMEDB_URL",
            "LYRACORE_DATABASE",
            "LYRACORE_COORDINATOR_TOKEN",
        ] {
            environment.insert(key.into(), required(key)?);
        }
        let url = url::Url::parse(&environment["LYRACORE_SPACETIMEDB_URL"])
            .context("LYRACORE_SPACETIMEDB_URL must be an HTTP URL")?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            bail!("LYRACORE_SPACETIMEDB_URL must be an HTTP URL");
        }
        for key in ["LYRACORE_SHARD_MAP", "LYRACORE_REALM_CORE"] {
            if let Ok(value) = std::env::var(key) {
                environment.insert(key.into(), value);
            }
        }
        Ok(Self::new(executable, environment))
    }

    pub async fn provision(&self, username: &str, password: &str, create_only: bool) -> Result<()> {
        if username.is_empty()
            || username.len() > 16
            || !username
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
            || password.is_empty()
            || password.len() > 16
            || !password
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        {
            bail!("invalid generated game credentials");
        }
        let mut command = Command::new(&self.executable);
        command
            .arg("provision")
            .arg(username)
            .arg("--password-stdin")
            .env_clear()
            .envs(&self.environment)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if create_only {
            command.arg("--create-only");
        }
        let mut child = command
            .spawn()
            .context("starting LyraCore account command")?;
        let result = tokio::time::timeout(Duration::from_secs(60), async {
            let mut stdin = child
                .stdin
                .take()
                .context("opening account command input")?;
            stdin.write_all(password.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
            stdin.shutdown().await?;
            drop(stdin);
            let status = child
                .wait()
                .await
                .context("waiting for LyraCore account command")?;
            if !status.success() {
                bail!("LyraCore account command failed; account credentials were not saved");
            }
            Ok(())
        })
        .await;
        match result {
            Ok(result) => result,
            Err(_) => {
                let _ = child.kill().await;
                bail!("LyraCore account command timed out; account credentials were not saved")
            }
        }
    }
}

fn required(name: &str) -> Result<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .with_context(|| format!("{name} is required for REALM_BACKEND=lyracore"))
}
