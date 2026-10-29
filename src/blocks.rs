use async_trait::async_trait;
use hashtree_core::{Hash, Store, StoreError, sha256, to_hex};
use std::{path::PathBuf, time::Duration};

const MAX_BLOCK: usize = 8 * 1024 * 1024;

/// Read-only adapter; never takes a second lock on an application's database.
pub struct Blocks {
    pub directory: Option<PathBuf>,
    pub daemon: Option<reqwest::Url>,
    client: reqwest::Client,
}

impl Blocks {
    pub fn new(
        directory: Option<PathBuf>,
        daemon: Option<reqwest::Url>,
        timeout: Duration,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            directory,
            daemon,
            client: reqwest::Client::builder()
                .timeout(timeout)
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .build()?,
        })
    }
}

fn failure(message: impl ToString) -> StoreError {
    StoreError::Other(message.to_string())
}

#[async_trait]
impl Store for Blocks {
    async fn get(&self, hash: &Hash) -> Result<Option<Vec<u8>>, StoreError> {
        let name = to_hex(hash);
        let mut bytes = None;
        if let Some(directory) = &self.directory {
            match crate::io::read_bounded(&directory.join(&name)) {
                Ok(value) => bytes = Some(value),
                Err(error)
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) => {}
                Err(error) => return Err(failure(error)),
            }
        }
        if bytes.is_none()
            && let Some(base) = &self.daemon
        {
            let url = base.join(&name).map_err(failure)?;
            let mut response = self.client.get(url).send().await.map_err(failure)?;
            if response.status() == reqwest::StatusCode::NOT_FOUND {
                return Err(failure("required index block is unavailable"));
            }
            response.error_for_status_ref().map_err(failure)?;
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(failure)? {
                if body.len() + chunk.len() > MAX_BLOCK {
                    return Err(failure("block exceeds 8 MiB"));
                }
                body.extend_from_slice(&chunk);
            }
            bytes = Some(body);
        }
        if let Some(body) = &bytes {
            if body.len() > MAX_BLOCK {
                return Err(failure("block exceeds 8 MiB"));
            }
            if sha256(body) != *hash {
                return Err(failure("block hash mismatch"));
            }
        }
        bytes
            .map(Some)
            .ok_or_else(|| failure("required index block is unavailable"))
    }
    async fn has(&self, hash: &Hash) -> Result<bool, StoreError> {
        Ok(self.get(hash).await?.is_some())
    }
    async fn put(&self, _: Hash, _: Vec<u8>) -> Result<bool, StoreError> {
        Err(failure("read-only block source"))
    }
    async fn delete(&self, _: &Hash) -> Result<bool, StoreError> {
        Err(failure("read-only block source"))
    }
}

/// A daemon is explicit and loopback-only; remote relays use the relay option.
pub fn daemon_url(value: &str) -> anyhow::Result<reqwest::Url> {
    let url = reqwest::Url::parse(value)?;
    anyhow::ensure!(
        url.scheme() == "http"
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.path() == "/"
            && url.host_str().is_some_and(|host| {
                host.trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
            }),
        "daemon must be an http:// loopback IP and port without a path or credentials"
    );
    Ok(url)
}
