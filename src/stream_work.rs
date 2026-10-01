//! One lazy stream reactor and bounded transport/UI queues per document.
use crate::action::ActionError;
use crate::lifecycle::Cancellation;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tokio::sync::{mpsc, OwnedSemaphorePermit, Semaphore};
use tokio_tungstenite::tungstenite::{protocol::WebSocketConfig, Message};

pub(crate) const MAX_STREAMS: usize = 8;
pub(crate) const MAX_EVENTS: usize = 64;
pub(crate) const EVENT_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MAX_OUTGOING: usize = 16;
pub(crate) const OUTGOING_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MAX_MESSAGE: usize = 8 * 1024 * 1024;
pub(crate) const MAX_URL: usize = 16 * 1024;
pub(crate) const SSE_LINE: usize = 64 * 1024;
pub(crate) const SSE_DATA: usize = 128 * 1024;
const SSE_FIELD: usize = 1024;

pub(crate) enum Data {
    Fields(Value),
    Binary(Vec<u8>),
}
impl Data {
    fn bytes(&self) -> usize {
        512 + match self {
            Self::Binary(bytes) => bytes.len(),
            Self::Fields(value) => value
                .as_object()
                .unwrap()
                .values()
                .filter_map(Value::as_str)
                .map(str::len)
                .sum(),
        }
    }
}

pub(crate) struct Queued {
    pub data: Data,
    slot: Arc<Slot>,
    _bytes: OwnedSemaphorePermit,
}
impl Queued {
    pub fn take(self) -> (i32, Data, OwnedSemaphorePermit) {
        let id = self.slot.id;
        // Release the retired stream's last reference before running its close
        // callback, so that callback can open a replacement at the stream limit.
        (id, self.data, self._bytes)
    }
}

struct Slot {
    id: i32,
    owner: Weak<Shared>,
}
impl Drop for Slot {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.upgrade() {
            owner.handles.lock().unwrap().remove(&self.id);
        }
    }
}

struct Handle {
    close: Cancellation,
    outgoing: Option<mpsc::Sender<Outgoing>>,
    buffered: Arc<AtomicUsize>,
}
struct Shared {
    handles: Mutex<HashMap<i32, Handle>>,
    event_bytes: Arc<Semaphore>,
    outgoing_bytes: Arc<Semaphore>,
    events: mpsc::Sender<Queued>,
    lifetime: Cancellation,
    wake: Arc<dyn Fn() + Send + Sync>,
}
pub(crate) struct StreamWork {
    shared: Arc<Shared>,
    sender: mpsc::Sender<Job>,
    pub events: mpsc::Receiver<Queued>,
}
enum Job {
    Socket {
        url: String,
        commands: mpsc::Receiver<Outgoing>,
        emitter: Emitter,
    },
    Source {
        url: String,
        emitter: Emitter,
    },
}
struct Emitter {
    shared: Arc<Shared>,
    slot: Arc<Slot>,
    close: Cancellation,
}
impl Emitter {
    async fn emit(&self, data: Data) -> Result<(), ()> {
        tokio::select! {
            biased;
            _ = self.close.cancelled() => Err(()),
            result = self.send(data, self.slot.clone()) => result,
        }
    }
    async fn send(&self, data: Data, slot: Arc<Slot>) -> Result<(), ()> {
        let bytes = data.bytes();
        if bytes > EVENT_BYTES {
            return Err(());
        }
        let delivery = async {
            let permit = self
                .shared
                .event_bytes
                .clone()
                .acquire_many_owned(bytes as u32)
                .await
                .map_err(|_| ())?;
            self.shared
                .events
                .send(Queued {
                    data,
                    slot,
                    _bytes: permit,
                })
                .await
                .map_err(|_| ())
        };
        tokio::select! {
            biased;
            _ = self.shared.lifetime.cancelled() => Err(()),
            result = delivery => { if result.is_ok() { (self.shared.wake)(); } result },
        }
    }
    async fn finish(self, data: Data) {
        // Do not cancel a terminal event merely because the caller requested
        // close. Document shutdown still cancels a blocked terminal delivery.
        let Emitter {
            shared,
            slot,
            close,
        } = self;
        close.cancel();
        let bytes = data.bytes();
        let delivery = async {
            let Ok(permit) = shared
                .event_bytes
                .clone()
                .acquire_many_owned(bytes as u32)
                .await
            else {
                return;
            };
            if shared
                .events
                .send(Queued {
                    data,
                    slot,
                    _bytes: permit,
                })
                .await
                .is_ok()
            {
                (shared.wake)();
            }
        };
        tokio::select! { biased; _ = shared.lifetime.cancelled() => {}, _ = delivery => {} }
    }
}

struct Outgoing {
    data: Option<Message>,
    bytes: usize,
    buffered: Arc<AtomicUsize>,
    _permit: OwnedSemaphorePermit,
}
impl Drop for Outgoing {
    fn drop(&mut self) {
        self.buffered.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

impl StreamWork {
    pub fn new(
        lifetime: Cancellation,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, ActionError> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .map_err(|error| ActionError::new("network_error", error.to_string()))?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(2)
            .build()
            .map_err(|error| ActionError::new("network_error", error.to_string()))?;
        let (sender, mut receiver) = mpsc::channel::<Job>(MAX_STREAMS);
        let (events, incoming) = mpsc::channel(MAX_EVENTS);
        let shared = Arc::new(Shared {
            handles: Mutex::new(HashMap::new()),
            event_bytes: Arc::new(Semaphore::new(EVENT_BYTES)),
            outgoing_bytes: Arc::new(Semaphore::new(OUTGOING_BYTES)),
            events,
            lifetime: lifetime.clone(),
            wake: Arc::new(wake),
        });
        std::thread::Builder::new().name("lapui-streams".into()).spawn(move || {
            runtime.block_on(async move {
                let mut tasks = tokio::task::JoinSet::new();
                loop {
                    tokio::select! {
                        biased;
                        _ = lifetime.cancelled() => break,
                        _ = tasks.join_next(), if !tasks.is_empty() => {},
                        job = receiver.recv(), if tasks.len() < MAX_STREAMS => {
                            let Some(job) = job else { break };
                            let client = client.clone();
                            tasks.spawn(async move { match job {
                                Job::Socket {url,commands,emitter} => socket(url,commands,emitter).await,
                                Job::Source {url,emitter} => source(url,client,emitter).await,
                            }});
                        }
                    }
                }
                tasks.abort_all(); while tasks.join_next().await.is_some() {}
            });
            runtime.shutdown_background();
        }).map_err(|error| ActionError::new("network_error", error.to_string()))?;
        Ok(Self {
            shared,
            sender,
            events: incoming,
        })
    }

    pub fn open(&self, id: i32, url: String, websocket: bool) -> Result<(), ActionError> {
        if self.shared.lifetime.is_cancelled() {
            return Err(ActionError::new("document_closed", "document is closed"));
        }
        if id <= 0 || url.len() > MAX_URL {
            return Err(ActionError::new(
                "invalid_arguments",
                "stream identity/URL exceeds its limits",
            ));
        }
        let parsed = reqwest::Url::parse(&url)
            .map_err(|_| ActionError::new("invalid_url", "stream requires an absolute URL"))?;
        if !(if websocket {
            matches!(parsed.scheme(), "ws" | "wss")
        } else {
            matches!(parsed.scheme(), "http" | "https")
        }) {
            return Err(ActionError::new(
                "invalid_url",
                "unsupported stream URL scheme",
            ));
        }
        let close = Cancellation::default();
        let (commands, receiving) = mpsc::channel(MAX_OUTGOING);
        let slot;
        {
            let mut handles = self.shared.handles.lock().unwrap();
            if handles.len() >= MAX_STREAMS {
                return Err(ActionError::new(
                    "network_busy",
                    "at most eight streams may be outstanding",
                ));
            }
            if handles.contains_key(&id) {
                return Err(ActionError::new(
                    "invalid_arguments",
                    "stream identity is already active",
                ));
            }
            slot = Arc::new(Slot {
                id,
                owner: Arc::downgrade(&self.shared),
            });
            handles.insert(
                id,
                Handle {
                    close: close.clone(),
                    outgoing: websocket.then_some(commands),
                    buffered: Arc::new(AtomicUsize::new(0)),
                },
            );
        }
        let emitter = Emitter {
            shared: self.shared.clone(),
            slot,
            close,
        };
        let job = if websocket {
            Job::Socket {
                url,
                commands: receiving,
                emitter,
            }
        } else {
            Job::Source { url, emitter }
        };
        self.sender
            .try_send(job)
            .map_err(|_| ActionError::new("network_busy", "stream reactor queue is full"))
    }
    pub fn close(&self, id: i32) {
        if let Some(handle) = self.shared.handles.lock().unwrap().get(&id) {
            handle.close.cancel();
        }
    }
    pub fn buffered(&self, id: i32) -> usize {
        self.shared
            .handles
            .lock()
            .unwrap()
            .get(&id)
            .map_or(0, |handle| handle.buffered.load(Ordering::Acquire))
    }
    pub fn send(&self, id: i32, data: Message) -> Result<(), ActionError> {
        let bytes = data.len();
        if bytes > MAX_MESSAGE {
            return Err(ActionError::new(
                "message_too_large",
                "WebSocket message exceeds 8 MiB",
            ));
        }
        let handles = self.shared.handles.lock().unwrap();
        let handle = handles
            .get(&id)
            .filter(|handle| !handle.close.is_cancelled())
            .ok_or_else(|| ActionError::new("stream_closed", "WebSocket is closed"))?;
        let sender = handle
            .outgoing
            .as_ref()
            .ok_or_else(|| ActionError::new("invalid_arguments", "EventSource is read-only"))?;
        let permit = self
            .shared
            .outgoing_bytes
            .clone()
            .try_acquire_many_owned(bytes.max(1) as u32)
            .map_err(|_| {
                ActionError::new("network_busy", "WebSocket outgoing byte budget is full")
            })?;
        handle.buffered.fetch_add(bytes, Ordering::AcqRel);
        sender
            .try_send(Outgoing {
                data: Some(data),
                bytes,
                buffered: handle.buffered.clone(),
                _permit: permit,
            })
            .map_err(|_| ActionError::new("network_busy", "WebSocket send queue is full"))
    }
    pub fn stats(&self) -> Value {
        json!({"streams":self.shared.handles.lock().unwrap().len(),"queuedEvents":self.events.len(),
            "queuedEventBytes":EVENT_BYTES-self.shared.event_bytes.available_permits(),
            "outgoingBytes":OUTGOING_BYTES-self.shared.outgoing_bytes.available_permits(),"reactorStarted":true})
    }
}

fn error_data(code: &str, message: impl Into<String>, sse: bool, fatal: bool) -> Data {
    let mut message = message.into();
    if message.len() > 4096 {
        let mut end = 4096;
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message.truncate(end);
    }
    Data::Fields(
        json!({"type":"error","kind":"error","code":code,"message":message,"fatal":fatal,"sse":sse}),
    )
}

async fn socket(url: String, commands: mpsc::Receiver<Outgoing>, emitter: Emitter) {
    let config = WebSocketConfig::default()
        .read_buffer_size(16 * 1024)
        .max_message_size(Some(MAX_MESSAGE))
        .max_frame_size(Some(MAX_MESSAGE))
        .max_write_buffer_size(MAX_MESSAGE + 128 * 1024);
    let connecting = tokio::time::timeout(
        Duration::from_secs(20),
        tokio_tungstenite::connect_async_with_config(&url, Some(config), false),
    );
    let connection = tokio::select! {biased;
        _=emitter.shared.lifetime.cancelled()=>return,
        _=emitter.close.cancelled()=>{emitter.finish(Data::Fields(json!({"type":"close","code":1000,"reason":"closed while connecting"}))).await;return;},
        result=connecting=>result.map_err(|_|"WebSocket connection timed out".to_owned()).and_then(|result|result.map_err(|error|error.to_string())),
    };
    let (websocket, _) = match connection {
        Ok(value) => value,
        Err(error) => {
            let _ = emitter
                .emit(error_data("network_error", error, false, false))
                .await;
            emitter
                .finish(Data::Fields(
                    json!({"type":"close","code":1006,"reason":"connection failed"}),
                ))
                .await;
            return;
        }
    };
    drive_socket(websocket, commands, emitter).await;
}

async fn drive_socket<S>(
    mut websocket: tokio_tungstenite::WebSocketStream<S>,
    mut commands: mpsc::Receiver<Outgoing>,
    emitter: Emitter,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    if emitter
        .emit(Data::Fields(json!({"type":"open"})))
        .await
        .is_err()
    {
        emitter
            .finish(Data::Fields(
                json!({"type":"close","code":1000,"reason":""}),
            ))
            .await;
        return;
    }
    let mut close_deadline = None;
    let mut code = 1000;
    let mut reason = String::new();
    loop {
        if emitter.close.is_cancelled() && close_deadline.is_none() {
            close_deadline = Some(tokio::time::Instant::now() + Duration::from_secs(1));
        }
        if let Some(deadline) = close_deadline {
            let closing = async {
                while let Ok(mut command) = commands.try_recv() {
                    websocket.send(command.data.take().unwrap()).await?;
                }
                websocket.close(None).await
            };
            tokio::select! {biased;_=emitter.shared.lifetime.cancelled()=>return,
                result=tokio::time::timeout_at(deadline,closing)=>{if !matches!(result,Ok(Ok(()))){code=1006;reason="close flush timed out or failed".into();}}
            }
            break;
        }
        tokio::select! {biased;
            _=emitter.shared.lifetime.cancelled()=>return,
            _=emitter.close.cancelled()=>{},
            command=commands.recv()=>{
                let Some(mut command)=command else {break};
                let sending=websocket.send(command.data.take().unwrap());tokio::pin!(sending);
                let result=tokio::select! {biased;
                    _=emitter.shared.lifetime.cancelled()=>return,
                    _=emitter.close.cancelled()=>{
                        let deadline=tokio::time::Instant::now()+Duration::from_secs(1);close_deadline=Some(deadline);
                        tokio::select! { biased;
                            _=emitter.shared.lifetime.cancelled()=>return,
                            result=tokio::time::timeout_at(deadline,&mut sending)=>result.map_err(|_|"WebSocket close flush timed out".to_owned()).and_then(|result|result.map_err(|error|error.to_string())),
                        }
                    },
                    result=&mut sending=>result.map_err(|error|error.to_string()),
                };
                if let Err(error)=result {code=1006;reason="send failed".into();let _=emitter.emit(error_data("network_error",error,false,false)).await;break;}
            },
            message=websocket.next()=>match message {
                Some(Ok(Message::Text(text)))=>{if emitter.emit(Data::Fields(json!({"type":"message","dataType":"text","data":text.as_str()}))).await.is_err() {continue;}},
                Some(Ok(Message::Binary(bytes)))=>{if emitter.emit(Data::Binary(bytes.to_vec())).await.is_err(){continue;}},
                Some(Ok(Message::Close(frame)))=>{if let Some(frame)=frame {code=u16::from(frame.code);reason=frame.reason.to_string();}break;},
                Some(Ok(_))=>{},
                Some(Err(error))=>{
                    let capacity=matches!(error,tokio_tungstenite::tungstenite::Error::Capacity(_));
                    code=if capacity {1009}else{1006};reason="receive failed".into();
                    let _=emitter.emit(error_data(if capacity {"message_too_large"}else{"network_error"},error.to_string(),false,false)).await;break;
                },None=>{code=1006;reason="connection ended".into();break;},
            }
        }
    }
    drop(websocket);
    drop(commands);
    emitter
        .finish(Data::Fields(
            json!({"type":"close","code":code,"reason":reason}),
        ))
        .await;
}

#[derive(Default)]
pub(crate) struct SseParser {
    line: Vec<u8>,
    skip_lf: bool,
    seen_first: bool,
    data: String,
    has_data: bool,
    event: Option<String>,
    pub last_event_id: String,
    pub retry_ms: Option<u64>,
}
pub(crate) struct SseEvent {
    pub event: String,
    pub data: String,
    pub last_event_id: String,
}
impl SseParser {
    pub fn push(&mut self, byte: u8) -> Result<Option<SseEvent>, String> {
        if self.skip_lf {
            self.skip_lf = false;
            if byte == b'\n' {
                return Ok(None);
            }
        }
        if byte == b'\r' || byte == b'\n' {
            let result = self.line();
            self.line.clear();
            self.skip_lf = byte == b'\r';
            result
        } else {
            if self.line.len() == SSE_LINE {
                return Err("SSE line exceeds 64 KiB".into());
            }
            self.line.push(byte);
            Ok(None)
        }
    }
    fn line(&mut self) -> Result<Option<SseEvent>, String> {
        if !self.seen_first {
            self.seen_first = true;
            if self.line.starts_with(&[0xEF, 0xBB, 0xBF]) {
                self.line.drain(..3);
            }
        }
        if self.line.is_empty() {
            if self.has_data {
                self.has_data = false;
                return Ok(Some(SseEvent {
                    event: self
                        .event
                        .take()
                        .filter(|name| !name.is_empty())
                        .unwrap_or_else(|| "message".into()),
                    data: std::mem::take(&mut self.data),
                    last_event_id: self.last_event_id.clone(),
                }));
            }
            self.event = None;
            return Ok(None);
        }
        if self.line[0] == b':' {
            return Ok(None);
        }
        let (field, value) = match self.line.iter().position(|byte| *byte == b':') {
            Some(index) => {
                let start = index + 1 + usize::from(self.line.get(index + 1) == Some(&b' '));
                (&self.line[..index], &self.line[start..])
            }
            None => (self.line.as_slice(), &[][..]),
        };
        let value = String::from_utf8_lossy(value);
        match field {
            b"data" => {
                let extra = value.len() + usize::from(self.has_data);
                if self.data.len() + extra > SSE_DATA {
                    return Err("SSE event data exceeds 128 KiB".into());
                }
                if self.has_data {
                    self.data.push('\n');
                }
                self.has_data = true;
                self.data.push_str(&value);
            }
            b"event" => {
                if value.len() > SSE_FIELD {
                    return Err("SSE event name exceeds 1 KiB".into());
                }
                self.event = Some(value.into_owned());
            }
            b"id" if !value.contains('\0') => {
                if value.len() > SSE_FIELD {
                    return Err("SSE event ID exceeds 1 KiB".into());
                }
                self.last_event_id = value.into_owned();
            }
            b"retry" if value.bytes().all(|byte| byte.is_ascii_digit()) => {
                self.retry_ms = value.parse().ok()
            }
            _ => {}
        }
        Ok(None)
    }
    #[cfg(test)]
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<SseEvent>, String> {
        let mut events = Vec::new();
        for byte in bytes {
            if let Some(event) = self.push(*byte)? {
                events.push(event);
            }
        }
        Ok(events)
    }
}

async fn source(url: String, client: reqwest::Client, emitter: Emitter) {
    let mut last_event_id = String::new();
    let mut retry = Duration::from_millis(3000);
    loop {
        let streaming = async {
            let mut request = client.get(&url).header("Accept", "text/event-stream");
            if !last_event_id.is_empty() {
                request = request.header("Last-Event-ID", &last_event_id);
            }
            let mut response = request
                .send()
                .await
                .map_err(|error| (false, error.to_string()))?;
            if response.status() == reqwest::StatusCode::NO_CONTENT {
                return Err((true, "EventSource server stopped the stream (204)".into()));
            }
            if !response.status().is_success() {
                return Err((
                    false,
                    format!("EventSource server returned HTTP {}", response.status()),
                ));
            }
            if !response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| {
                    value
                        .split(';')
                        .next()
                        .unwrap_or_default()
                        .trim()
                        .eq_ignore_ascii_case("text/event-stream")
                })
            {
                return Err((true, "EventSource response is not text/event-stream".into()));
            }
            if emitter
                .emit(Data::Fields(
                    json!({"type":"open","kind":"open","data":"","lastEventId":last_event_id}),
                ))
                .await
                .is_err()
            {
                return Ok(());
            }
            let mut parser = SseParser {
                last_event_id: last_event_id.clone(),
                ..Default::default()
            };
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|error| (false, error.to_string()))?
            {
                for byte in chunk {
                    if let Some(event) = parser.push(byte).map_err(|error| (true, error))? {
                        if emitter.emit(Data::Fields(json!({"type":event.event,"kind":"message","data":event.data,"lastEventId":event.last_event_id}))).await.is_err(){return Ok(());}
                    }
                    if byte == b'\r' || byte == b'\n' {
                        last_event_id.clone_from(&parser.last_event_id);
                        if let Some(ms) = parser.retry_ms.take() {
                            retry = Duration::from_millis(ms.clamp(250, 30000));
                        }
                    }
                }
            }
            Ok::<(), (bool, String)>(())
        };
        let result = tokio::select! {biased;_=emitter.shared.lifetime.cancelled()=>return,_=emitter.close.cancelled()=>return,result=streaming=>result};
        if emitter.close.is_cancelled() {
            return;
        }
        match result {
            Err((true, error)) => {
                let code = if error.starts_with("SSE ") {
                    "message_too_large"
                } else {
                    "network_error"
                };
                emitter.finish(error_data(code, error, true, true)).await;
                return;
            }
            other => {
                let message = other.err().map_or_else(
                    || "EventSource connection closed".to_owned(),
                    |(_, error)| error,
                );
                if emitter
                    .emit(error_data("network_error", message, true, false))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        }
        tokio::select! {biased;_=emitter.shared.lifetime.cancelled()=>return,_=emitter.close.cancelled()=>return,_=tokio::time::sleep(retry)=>{}}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (StreamWork, Emitter, mpsc::Receiver<Outgoing>) {
        let (events, incoming) = mpsc::channel(MAX_EVENTS);
        let (outgoing, commands) = mpsc::channel(MAX_OUTGOING);
        let shared = Arc::new(Shared {
            handles: Mutex::new(HashMap::new()),
            event_bytes: Arc::new(Semaphore::new(EVENT_BYTES)),
            outgoing_bytes: Arc::new(Semaphore::new(OUTGOING_BYTES)),
            events,
            lifetime: Cancellation::default(),
            wake: Arc::new(|| {}),
        });
        let close = Cancellation::default();
        shared.handles.lock().unwrap().insert(
            1,
            Handle {
                close: close.clone(),
                outgoing: Some(outgoing),
                buffered: Arc::new(AtomicUsize::new(0)),
            },
        );
        let emitter = Emitter {
            shared: shared.clone(),
            close,
            slot: Arc::new(Slot {
                id: 1,
                owner: Arc::downgrade(&shared),
            }),
        };
        let (sender, _) = mpsc::channel(MAX_STREAMS);
        (
            StreamWork {
                shared,
                sender,
                events: incoming,
            },
            emitter,
            commands,
        )
    }

    fn copy_emitter(emitter: &Emitter) -> Emitter {
        Emitter {
            shared: emitter.shared.clone(),
            close: emitter.close.clone(),
            slot: emitter.slot.clone(),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn delivery_backpressure_preserves_order_and_terminal_slot_until_consumed() {
        let (mut work, emitter, _) = fixture();
        for index in 0..MAX_EVENTS {
            emitter
                .emit(Data::Fields(json!({"sequence":index})))
                .await
                .unwrap();
        }
        let pending_emitter = copy_emitter(&emitter);
        let mut pending = tokio::spawn(async move {
            pending_emitter
                .emit(Data::Fields(json!({"sequence":MAX_EVENTS})))
                .await
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut pending)
                .await
                .is_err()
        );
        assert_eq!(work.events.len(), MAX_EVENTS);
        let first = work.events.recv().await.unwrap();
        assert!(matches!(&first.data, Data::Fields(value) if value["sequence"] == 0));
        drop(first);
        pending.await.unwrap().unwrap();
        for index in 1..=MAX_EVENTS {
            let record = work.events.recv().await.unwrap();
            assert!(matches!(&record.data, Data::Fields(value) if value["sequence"] == index));
        }
        assert_eq!(work.stats()["queuedEventBytes"], 0);
        emitter.finish(Data::Fields(json!({"type":"close"}))).await;
        assert_eq!(work.stats()["streams"], 1);
        assert_eq!(
            work.send(1, Message::Text("late".into())).unwrap_err().code,
            "stream_closed"
        );
        let (_, data, bytes) = work.events.recv().await.unwrap().take();
        assert!(matches!(data, Data::Fields(value) if value["type"] == "close"));
        // Slot released before callback, byte permit retained through callback.
        assert_eq!(work.stats()["streams"], 0);
        assert_ne!(work.stats()["queuedEventBytes"], 0);
        drop(bytes);
        assert_eq!(work.stats()["queuedEventBytes"], 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn event_byte_budget_and_close_interrupt_blocked_delivery() {
        let (mut work, emitter, _) = fixture();
        emitter
            .emit(Data::Binary(vec![1; MAX_MESSAGE]))
            .await
            .unwrap();
        let pending_emitter = copy_emitter(&emitter);
        let mut pending = tokio::spawn(async move {
            pending_emitter
                .emit(Data::Binary(vec![2; MAX_MESSAGE]))
                .await
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut pending)
                .await
                .is_err()
        );
        assert_eq!(work.events.len(), 1);
        // Tokio's fair semaphore reserves available bytes for a pending
        // acquire_many; those reservations are included in transport stats.
        assert_eq!(work.stats()["queuedEventBytes"], EVENT_BYTES);
        work.close(1);
        assert!(tokio::time::timeout(Duration::from_secs(1), pending)
            .await
            .unwrap()
            .unwrap()
            .is_err());
        assert_eq!(work.stats()["queuedEventBytes"], MAX_MESSAGE + 512);
        drop(work.events.recv().await.unwrap());
        assert_eq!(work.stats()["queuedEventBytes"], 0);
        // Document cancellation also interrupts a terminal delivery at capacity.
        for _ in 0..MAX_EVENTS {
            emitter
                .send(
                    Data::Fields(json!({"type":"message"})),
                    emitter.slot.clone(),
                )
                .await
                .unwrap();
        }
        let lifetime = work.shared.lifetime.clone();
        let mut terminal = tokio::spawn(emitter.finish(Data::Fields(json!({"type":"close"}))));
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut terminal)
                .await
                .is_err()
        );
        lifetime.cancel();
        tokio::time::timeout(Duration::from_secs(1), terminal)
            .await
            .unwrap()
            .unwrap();
        while let Ok(record) = work.events.try_recv() {
            drop(record);
        }
        assert_eq!(work.stats()["streams"], 0);
        assert_eq!(work.stats()["queuedEventBytes"], 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn combined_stream_limit_includes_jobs_and_undelivered_terminal_events() {
        let (mut work, emitter, _) = fixture();
        let (sender, receiving) = mpsc::channel(MAX_STREAMS);
        work.sender = sender;
        assert_eq!(
            work.open(2, "file:///bad".into(), false).unwrap_err().code,
            "invalid_url"
        );
        assert_eq!(work.stats()["streams"], 1);
        for id in 2..=MAX_STREAMS as i32 {
            let websocket = id % 2 == 0;
            work.open(
                id,
                format!(
                    "{}://127.0.0.1:1/test",
                    if websocket { "ws" } else { "http" }
                ),
                websocket,
            )
            .unwrap();
        }
        work.close(2);
        assert_eq!(
            work.open(9, "http://127.0.0.1:1/new".into(), false)
                .unwrap_err()
                .code,
            "network_busy"
        );
        emitter.finish(Data::Fields(json!({"type":"close"}))).await;
        assert_eq!(work.stats()["streams"], MAX_STREAMS);
        drop(work.events.recv().await.unwrap().take());
        assert_eq!(work.stats()["streams"], MAX_STREAMS - 1);
        work.open(9, "http://127.0.0.1:1/new".into(), false)
            .unwrap();
        drop(receiving); // Reclaims never-started jobs and their slots.
        assert_eq!(work.stats()["streams"], 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn document_cancellation_interrupts_a_proven_blocked_write_during_graceful_close() {
        let (mut work, emitter, commands) = fixture();
        // A bounded duplex transport deterministically blocks the frame flush;
        // TCP loopback buffers vary by OS and can absorb the entire byte budget.
        let (client, _unread_server) = tokio::io::duplex(1024);
        let socket = tokio_tungstenite::WebSocketStream::from_raw_socket(
            client,
            tokio_tungstenite::tungstenite::protocol::Role::Client,
            None,
        )
        .await;
        work.send(1, Message::Binary(vec![0; MAX_MESSAGE].into()))
            .unwrap();
        let mut driver = tokio::spawn(drive_socket(socket, commands, emitter));
        assert!(tokio::time::timeout(Duration::from_millis(10), &mut driver)
            .await
            .is_err());
        assert_eq!(work.buffered(1), MAX_MESSAGE);
        assert_eq!(work.stats()["outgoingBytes"], MAX_MESSAGE);
        work.close(1);
        // Prove the close grace period is still flushing the blocked frame.
        assert!(tokio::time::timeout(Duration::from_millis(10), &mut driver)
            .await
            .is_err());
        work.shared.lifetime.cancel();
        tokio::time::timeout(Duration::from_millis(100), driver)
            .await
            .unwrap()
            .unwrap();
        while let Ok(event) = work.events.try_recv() {
            drop(event);
        }
        assert_eq!(work.stats()["streams"], 0);
        assert_eq!(work.stats()["outgoingBytes"], 0);
        assert_eq!(work.stats()["queuedEventBytes"], 0);
    }

    #[test]
    fn outgoing_limits_restore_permits_after_rejection_inflight_drop_and_close() {
        let (work, _emitter, mut commands) = fixture();
        for _ in 0..MAX_OUTGOING {
            work.send(1, Message::Text("test".into())).unwrap();
        }
        assert_eq!(work.buffered(1), MAX_OUTGOING * 4);
        assert_eq!(
            work.send(1, Message::Text("overflow".into()))
                .unwrap_err()
                .code,
            "network_busy"
        );
        assert_eq!(work.buffered(1), MAX_OUTGOING * 4);
        let sending = commands.try_recv().unwrap();
        work.send(1, Message::Text("next".into())).unwrap();
        assert_eq!(work.buffered(1), (MAX_OUTGOING + 1) * 4);
        work.close(1); // Independent of an already full outgoing queue.
        assert_eq!(
            work.send(1, Message::Text("late".into())).unwrap_err().code,
            "stream_closed"
        );
        drop(sending);
        drop(commands);
        assert_eq!(work.buffered(1), 0);
        assert_eq!(work.stats()["outgoingBytes"], 0);

        let (work, _emitter, mut commands) = fixture();
        work.send(1, Message::Binary(vec![0; MAX_MESSAGE].into()))
            .unwrap();
        let inflight = commands.try_recv().unwrap();
        work.send(1, Message::Binary(vec![0; MAX_MESSAGE].into()))
            .unwrap();
        assert_eq!(work.stats()["outgoingBytes"], OUTGOING_BYTES);
        assert_eq!(
            work.send(1, Message::Text("a".into())).unwrap_err().code,
            "network_busy"
        );
        assert_eq!(work.buffered(1), OUTGOING_BYTES);
        assert_eq!(
            work.send(1, Message::Binary(vec![0; MAX_MESSAGE + 1].into()))
                .unwrap_err()
                .code,
            "message_too_large"
        );
        drop(inflight);
        drop(commands);
        assert_eq!(work.stats()["outgoingBytes"], 0);
    }

    #[test]
    fn sse_limits_bound_unterminated_lines_events_and_metadata() {
        let mut parser = SseParser::default();
        assert!(parser.feed(&vec![b'x'; SSE_LINE]).unwrap().is_empty());
        assert!(parser.push(b'x').is_err());
        let mut parser = SseParser::default();
        // Many individually short data fields can form an oversized event.
        for _ in 0..SSE_DATA {
            parser.feed(b"data:\n").unwrap();
        }
        assert_eq!(parser.data.len(), SSE_DATA - 1);
        parser.feed(b"data:\n").unwrap();
        assert!(parser.feed(b"data:\n").is_err());
        for field in ["event", "id"] {
            let mut parser = SseParser::default();
            assert!(parser
                .feed(format!("{field}:{}\n", "x".repeat(SSE_FIELD + 1)).as_bytes())
                .is_err());
        }
        let mut parser = SseParser::default();
        let events = parser.feed(b"id: first\rid:\nevent:\ndata:\n\n").unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "message");
        assert_eq!(events[0].data, "");
        assert_eq!(events[0].last_event_id, "");
        assert_eq!(parser.last_event_id, "");
    }
}
