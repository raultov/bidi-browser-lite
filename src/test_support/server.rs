//! A tiny TCP accept loop shared by the mocks: one task per connection,
//! every task aborted when the server is dropped.

use std::future::Future;
use std::io;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use tokio::net::{TcpListener, TcpStream};
use tokio::task::{AbortHandle, JoinHandle};

/// Live connection tasks, so they can be dropped on demand.
#[derive(Default)]
struct Connections {
    tasks: Mutex<Vec<AbortHandle>>,
    accepted: AtomicUsize,
    next_id: AtomicU64,
}

impl Connections {
    fn push(&self, handle: AbortHandle) {
        let mut tasks = self.tasks.lock().unwrap_or_else(PoisonError::into_inner);
        tasks.retain(|t| !t.is_finished());
        tasks.push(handle);
    }

    fn abort_all(&self) {
        let tasks = std::mem::take(&mut *self.tasks.lock().unwrap_or_else(PoisonError::into_inner));
        tasks.iter().for_each(AbortHandle::abort);
    }
}

/// A listening server whose connections are served by `handler`.
pub(crate) struct Server {
    port: u16,
    accept: JoinHandle<()>,
    connections: Arc<Connections>,
}

impl Server {
    /// Binds `127.0.0.1:port` (0 = any) and serves each connection with
    /// `handler(stream, connection_id)`.
    pub(crate) async fn start<F, Fut>(port: u16, handler: F) -> io::Result<Self>
    where
        F: Fn(TcpStream, u64) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let listener = TcpListener::bind(("127.0.0.1", port)).await?;
        let port = listener.local_addr()?.port();
        let connections = Arc::new(Connections::default());
        let accept = tokio::spawn(accept_loop(listener, Arc::clone(&connections), handler));
        Ok(Self {
            port,
            accept,
            connections,
        })
    }

    /// The bound port.
    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    /// How many connections were accepted so far.
    pub(crate) fn connection_count(&self) -> usize {
        self.connections.accepted.load(Ordering::SeqCst)
    }

    /// Closes every live connection; the server keeps listening.
    pub(crate) fn drop_connections(&self) {
        self.connections.abort_all();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.accept.abort();
        self.connections.abort_all();
    }
}

async fn accept_loop<F, Fut>(listener: TcpListener, connections: Arc<Connections>, handler: F)
where
    F: Fn(TcpStream, u64) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    while let Ok((stream, _)) = listener.accept().await {
        connections.accepted.fetch_add(1, Ordering::SeqCst);
        let id = connections.next_id.fetch_add(1, Ordering::SeqCst);
        let task = tokio::spawn(handler(stream, id));
        connections.push(task.abort_handle());
    }
}
