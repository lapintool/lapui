//! One lazy, document-owned HTTP reactor; bounded by the UI's outstanding map.
use crate::lifecycle::Cancellation;
use std::path::PathBuf;
use std::time::Duration;

pub(crate) const MAX_OUTSTANDING: usize = 16;
pub(crate) const MAX_ACTIVE: usize = 8;

pub(crate) struct FetchRequest {
    pub input: String,
    pub base_url: Option<String>,
    pub app_root: Option<PathBuf>,
    pub cancellation: Cancellation,
    pub complete: Box<dyn FnOnce(Result<serde_json::Value, String>) + Send>,
}

pub(crate) struct FetchWork {
    sender: tokio::sync::mpsc::Sender<FetchRequest>,
}

impl FetchWork {
    pub fn new(lifetime: Cancellation) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .map_err(|error| error.to_string())?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?;
        let (sender, mut receiver) = tokio::sync::mpsc::channel::<FetchRequest>(MAX_OUTSTANDING);
        std::thread::Builder::new()
            .name("lapui-fetch".into())
            .spawn(move || runtime.block_on(async move {
                let mut tasks = tokio::task::JoinSet::new();
                let permits = std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_ACTIVE));
                loop {
                    tokio::select! {
                        biased;
                        _ = lifetime.cancelled() => break,
                        _ = tasks.join_next(), if !tasks.is_empty() => {},
                        request = receiver.recv(), if tasks.len() < MAX_OUTSTANDING => {
                            let Some(request) = request else { break };
                            let client = client.clone();
                            let permits = permits.clone();
                            tasks.spawn(async move {
                                let result = tokio::select! {
                                    biased;
                                    _ = request.cancellation.cancelled() => Err("fetch aborted".into()),
                                    result = async {
                                        let _permit = permits.acquire_owned().await.map_err(|error| error.to_string())?;
                                        crate::runtime::perform_fetch(
                                            &request.input, request.base_url.as_deref(),
                                            request.app_root.as_deref(), &client,
                                        ).await
                                    } => result,
                                };
                                (request.complete)(result);
                            });
                        }
                    }
                }
                tasks.abort_all();
                while tasks.join_next().await.is_some() {}
            }))
            .map_err(|error| error.to_string())?;
        Ok(Self { sender })
    }

    pub fn submit(&self, request: FetchRequest) -> Result<(), String> {
        self.sender
            .try_send(request)
            .map_err(|error| error.to_string())
    }
}
