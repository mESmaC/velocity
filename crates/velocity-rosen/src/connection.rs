//! The pinned Rosen client owns the writer and spawns a detached reader. Dropping
//! the writer does not close the duplex pipe while that reader still holds it.

use std::collections::HashMap;
use std::future::Future;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, LazyLock, Mutex};

use rosen_client::{Client, Endpoint, Error};
use tokio::sync::{Mutex as AsyncMutex, oneshot};

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

type Entry = Arc<AsyncMutex<Option<Arc<Connection>>>>;
static REQUESTS: LazyLock<Mutex<HashMap<String, Entry>>> = LazyLock::new(Mutex::default);

pub(crate) fn forget(endpoint: &Endpoint) {
    if let Ok(mut entries) = REQUESTS.lock() {
        entries.remove(endpoint.as_str());
    }
}

/// Ordinary requests share a connection across callers. Event streams use a
/// dedicated connection because a subscription replaces its predecessor.
pub async fn requests(endpoint: &Endpoint) -> Result<Arc<Connection>, Error> {
    let entry = {
        let mut entries = REQUESTS.lock().unwrap_or_else(|error| error.into_inner());
        Arc::clone(entries.entry(endpoint.to_string()).or_default())
    };
    let mut entry = entry.lock().await;
    if let Some(connection) = entry.as_ref() {
        match tokio::time::timeout(std::time::Duration::from_secs(10), connection.ping()).await {
            Ok(Ok(())) => return Ok(Arc::clone(connection)),
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
    *entry = None;
    let connection = Arc::new(Connection::dedicated(endpoint).await?);
    *entry = Some(Arc::clone(&connection));
    Ok(connection)
}
