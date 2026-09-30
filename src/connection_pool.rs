//! Fixed workers and a bounded connection queue for the development TCP adapter.
use std::io::{self, Write};
use std::net::TcpStream;
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};

pub(crate) const WORKERS: usize = 8;
pub(crate) const QUEUE: usize = 64;

pub(crate) struct ConnectionPool {
    sender: Option<mpsc::SyncSender<TcpStream>>,
    workers: Vec<JoinHandle<()>>,
}

impl ConnectionPool {
    pub fn new(handler: impl Fn(TcpStream) + Send + Sync + 'static) -> io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(QUEUE);
        let receiver = Arc::new(Mutex::new(receiver));
        let handler = Arc::new(handler);
        let mut workers = Vec::new();
        for _ in 0..WORKERS {
            let receiver = receiver.clone();
            let handler = handler.clone();
            match thread::Builder::new()
                .name("lapui-control".into())
                .spawn(move || loop {
                    let next = receiver.lock().unwrap().recv();
                    let Ok(stream) = next else { break };
                    handler(stream);
                }) {
                Ok(worker) => workers.push(worker),
                Err(error) => {
                    drop(sender);
                    for worker in workers {
                        let _ = worker.join();
                    }
                    return Err(error);
                }
            }
        }
        Ok(Self {
            sender: Some(sender),
            workers,
        })
    }

    pub fn submit(&self, stream: TcpStream) {
        if let Err(error) = self.sender.as_ref().unwrap().try_send(stream) {
            let mut stream = match error {
                mpsc::TrySendError::Full(stream) | mpsc::TrySendError::Disconnected(stream) => {
                    stream
                }
            };
            // Reject without blocking the acceptor or allocating another worker.
            if stream.set_nonblocking(true).is_ok() {
                let _ = writeln!(stream, "{{\"ok\":false,\"error\":{{\"code\":\"control_busy\",\"message\":\"Control connection queue is full\"}}}}");
            }
        }
    }
}

impl Drop for ConnectionPool {
    fn drop(&mut self) {
        self.sender.take();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[test]
    fn fixed_workers_bound_connections_reject_overflow_and_join_after_work_finishes() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let (released, release) = mpsc::channel();
        let release = Arc::new(Mutex::new(release));
        let (started, observed) = mpsc::channel();
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let pool = ConnectionPool::new({
            let active = active.clone();
            let maximum = maximum.clone();
            move |_stream| {
                let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                maximum.fetch_max(current, Ordering::SeqCst);
                let _ = started.send(());
                let _ = release.lock().unwrap().recv();
                active.fetch_sub(1, Ordering::SeqCst);
            }
        })
        .unwrap();
        struct ReleaseAll(mpsc::Sender<()>);
        impl Drop for ReleaseAll {
            fn drop(&mut self) {
                for _ in 0..WORKERS + QUEUE {
                    let _ = self.0.send(());
                }
            }
        }
        let release_guard = ReleaseAll(released);
        let mut clients = Vec::new();
        for _ in 0..WORKERS {
            clients.push(TcpStream::connect(listener.local_addr().unwrap()).unwrap());
            pool.submit(listener.accept().unwrap().0);
            observed.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        for _ in 0..QUEUE {
            clients.push(TcpStream::connect(listener.local_addr().unwrap()).unwrap());
            pool.submit(listener.accept().unwrap().0);
        }
        let rejected = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        rejected
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        pool.submit(listener.accept().unwrap().0);
        let mut reply = String::new();
        BufReader::new(rejected).read_line(&mut reply).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&reply).unwrap()["error"]["code"],
            "control_busy"
        );
        assert_eq!(maximum.load(Ordering::SeqCst), WORKERS);
        drop(release_guard);
        drop(pool);
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }
}
