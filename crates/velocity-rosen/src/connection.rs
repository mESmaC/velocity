//! The pinned Rosen client owns the writer and spawns a detached reader. Dropping
//! the writer does not close the duplex pipe while that reader still holds it.

use std::collections::HashMap;
use std::future::Future;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use rosen_client::{Client, Endpoint, Error};
use tokio::sync::{Mutex as AsyncMutex, oneshot};

/// Skip the synchronous `ping()` check when a shared connection was verified recently (#541).
const VERIFY_INTERVAL: Duration = Duration::from_secs(5);

/// Owns the runtime of a Rosen connection (or an operation that opens its own
/// connection). Dropping the scope cancels its detached tasks and closes their I/O.
pub struct Scoped<Value> {
    pub(crate) value: Value,
    pub(crate) stop: oneshot::Sender<()>,
}

impl<Value: Send + 'static> Scoped<Value> {
    pub(crate) async fn start<Opened: Future<Output = Result<Value, Error>>>(
        open: impl FnOnce() -> Opened + Send + 'static,
    ) -> Result<Self, Error> {
        let (opened, result) = oneshot::channel();
        let (stop, done) = oneshot::channel();
        std::thread::Builder::new()
            .name("rosen-connection".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = opened.send(Err(error.into()));
                        return;
                    }
                };
                runtime.block_on(async {
                    tokio::select! {
                        _ = async {
                            let value = open().await;
                            let connected = value.is_ok();
                            if opened.send(value).is_ok() && connected {
                                std::future::pending::<()>().await;
                            }
                        } => {}
                        _ = done => {}
                    }
                });
            })?;
        Ok(Self {
            value: result.await.map_err(|_| Error::Disconnected)??,
            stop,
        })
    }
}

impl<Value> Deref for Scoped<Value> {
    type Target = Value;

    fn deref(&self) -> &Value {
        &self.value
    }
}

impl<Value> DerefMut for Scoped<Value> {
    fn deref_mut(&mut self) -> &mut Value {
        &mut self.value
    }
}

pub type Connection = Scoped<Arc<Client>>;

impl Connection {
    pub async fn dedicated(endpoint: &Endpoint) -> Result<Self, Error> {
        let endpoint = endpoint.clone();
        Self::start(move || async move { Client::connect(&endpoint).await.map(Arc::new) }).await
    }

    pub(crate) fn client(&self) -> Arc<Client> {
        Arc::clone(&self.value)
    }
}

struct CachedConnection {
    connection: Arc<Connection>,
    verified_at: Instant,
}

type Entry = Arc<AsyncMutex<Option<CachedConnection>>>;
static REQUESTS: LazyLock<Mutex<HashMap<String, Entry>>> = LazyLock::new(Mutex::default);

pub(crate) fn forget(endpoint: &Endpoint) {
    if let Ok(mut entries) = REQUESTS.lock() {
        entries.remove(endpoint.as_str());
    }
}

/// Drops a cached shared connection if it is still the active entry for `endpoint`,
/// forcing the next caller to open a fresh pipe immediately.
pub async fn invalidate(endpoint: &Endpoint, failed: &Arc<Connection>) {
    let entry = {
        let entries = REQUESTS.lock().unwrap_or_else(|error| error.into_inner());
        entries.get(endpoint.as_str()).cloned()
    };
    if let Some(entry) = entry {
        let mut guard = entry.lock().await;
        if guard
            .as_ref()
            .is_some_and(|cached| Arc::ptr_eq(&cached.connection, failed))
        {
            *guard = None;
        }
    }
}

/// Ordinary requests share a connection across callers. Event streams use a
/// dedicated connection because a subscription replaces its predecessor.
pub async fn requests(endpoint: &Endpoint) -> Result<Arc<Connection>, Error> {
    requests_timed(endpoint)
        .await
        .map(|(connection, _)| connection)
}

/// Like [`requests`], returning the time spent connecting or verifying the Rosen connection.
pub async fn requests_timed(endpoint: &Endpoint) -> Result<(Arc<Connection>, Duration), Error> {
    let started = Instant::now();
    let entry = {
        let mut entries = REQUESTS.lock().unwrap_or_else(|error| error.into_inner());
        Arc::clone(entries.entry(endpoint.to_string()).or_default())
    };
    let mut guard = entry.lock().await;
    if let Some(cached) = guard.as_mut() {
        if cached.verified_at.elapsed() < VERIFY_INTERVAL {
            return Ok((Arc::clone(&cached.connection), started.elapsed()));
        }
        match tokio::time::timeout(Duration::from_secs(10), cached.connection.ping()).await {
            Ok(Ok(())) => {
                cached.verified_at = Instant::now();
                return Ok((Arc::clone(&cached.connection), started.elapsed()));
            }
            Ok(Err(_)) => {}
            Err(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "Rosen did not answer its connection check",
                )
                .into());
            }
        }
    }
    *guard = None;
    let connection = Arc::new(Connection::dedicated(endpoint).await?);
    *guard = Some(CachedConnection {
        connection: Arc::clone(&connection),
        verified_at: Instant::now(),
    });
    Ok((connection, started.elapsed()))
}
