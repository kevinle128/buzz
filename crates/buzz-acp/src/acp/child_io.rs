//! Reader and writer tasks that own the stdio of one agent child.
//!
//! The reader task reads the child's stdout at all times, also between
//! turns. It sends each `_buzz/wake` request to the pool and every other
//! line to the [`LineReceiver`] that the ACP read loops consume, so the read
//! loops see the same lines and errors that they read from the pipe before.
//! The writer task is the only writer of the child's stdin. Every frame goes
//! through its queue, so frames never interleave, and each caller gets the
//! result of its own write.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot, Notify, OwnedSemaphorePermit, Semaphore};
use tokio_util::codec::{FramedRead, LinesCodec, LinesCodecError};

use super::wake;

/// Lines that can wait between the reader task and the ACP read loop.
const LINE_QUEUE_LINES: usize = 256;
/// Bytes that can wait between the reader task and the ACP read loop.
const LINE_QUEUE_BYTES: usize = 32 * 1024 * 1024;
/// While no read loop runs, a notification line is kept only if this many
/// line slots stay free after it. The free slots are for lines with an `id`.
const IDLE_RESERVE_LINES: usize = 32;
/// While no read loop runs, a notification line is kept only if this many
/// bytes stay free after it. The free bytes are for lines with an `id`.
const IDLE_RESERVE_BYTES: usize = 16 * 1024 * 1024;
/// Frames that can wait for the writer task.
const WRITE_QUEUE_FRAMES: usize = 64;
/// Wake requests of one child that can wait for their response at a time.
/// The reader stops reading that child's stdout while all are taken.
const WAKES_IN_FLIGHT_PER_CHILD: usize = 8;
/// Smallest interval between two warnings about dropped notification lines.
const DROP_WARN_INTERVAL: Duration = Duration::from_secs(60);
/// Upper bound for writing one wake response to the child's stdin.
const WAKE_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

// A line of the maximum size must always fit, also into the reserve that
// lines with an `id` keep while the client is idle.
const _: () = assert!(IDLE_RESERVE_BYTES >= super::MAX_LINE_SIZE);
const _: () = assert!(LINE_QUEUE_BYTES > IDLE_RESERVE_BYTES);
const _: () = assert!(LINE_QUEUE_LINES > IDLE_RESERVE_LINES);

/// Source of the unique generation number of each child.
static NEXT_CHILD_GENERATION: AtomicU64 = AtomicU64::new(1);

fn stdin_closed() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::BrokenPipe,
        "ACP stdin closed by an incomplete or failed frame write",
    )
}

/// One stdout item and the queue bytes that it holds until it is read.
struct LineItem {
    line: Result<String, LinesCodecError>,
    _bytes: OwnedSemaphorePermit,
}

/// State that the reader task and the read loops share.
#[derive(Default)]
struct ReadState {
    /// Number of read loops that run now. Zero means the client is idle.
    readers: AtomicUsize,
    /// Wakes a reader that waits for queue space when the client goes idle,
    /// so that it can drop a notification line instead of waiting.
    went_idle: Notify,
    /// Notification lines dropped while the client was idle.
    dropped: AtomicU64,
}

impl ReadState {
    fn is_idle(&self) -> bool {
        self.readers.load(Ordering::Acquire) == 0
    }
}

/// Marks a read loop as running while it is alive.
pub(crate) struct ReadingGuard(Arc<ReadState>);

impl Drop for ReadingGuard {
    fn drop(&mut self) {
        if self.0.readers.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.0.went_idle.notify_one();
        }
    }
}

/// The child's stdout lines, as the reader task forwards them.
pub(crate) struct LineReceiver {
    rx: mpsc::Receiver<LineItem>,
    state: Arc<ReadState>,
}

impl LineReceiver {
    /// The next stdout item, as `FramedRead::next` gave it. `None` means
    /// that stdout is closed. Cancel-safe.
    pub(crate) async fn next(&mut self) -> Option<Result<String, LinesCodecError>> {
        self.rx.recv().await.map(|item| item.line)
    }

    /// Mark a read loop as running until the guard drops. While no read loop
    /// runs, the reader task may drop notification lines that do not fit.
    pub(crate) fn reading(&self) -> ReadingGuard {
        self.state.readers.fetch_add(1, Ordering::AcqRel);
        ReadingGuard(Arc::clone(&self.state))
    }

    /// Notification lines dropped while the client was idle.
    pub(crate) fn dropped_notifications(&self) -> u64 {
        self.state.dropped.load(Ordering::Relaxed)
    }
}

struct WriteJob {
    line: String,
    done: oneshot::Sender<std::io::Result<()>>,
}

/// Handle to the writer task of one child's stdin.
#[derive(Clone)]
pub(crate) struct StdinWriter {
    tx: mpsc::Sender<WriteJob>,
}

impl StdinWriter {
    /// Write `line` and a newline as one frame and wait for the result.
    ///
    /// When this future drops before the frame starts, nothing is written.
    /// When it drops while the frame is partly written, the writer task
    /// closes stdin, so a partial frame is never followed by other bytes.
    /// After a failed or closed frame, every later write fails with
    /// `BrokenPipe`.
    pub(crate) async fn write_line(&self, line: String) -> std::io::Result<()> {
        let (done, result) = oneshot::channel();
        self.tx
            .send(WriteJob { line, done })
            .await
            .map_err(|_| stdin_closed())?;
        result.await.map_err(|_| stdin_closed())?
    }

    /// Whether stdin is still open for writes.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn is_open(&self) -> bool {
        !self.tx.is_closed()
    }
}

async fn run_writer<W: AsyncWrite + Unpin>(stdin: W, mut jobs: mpsc::Receiver<WriteJob>) {
    let mut slot = Some(stdin);
    while let Some(mut job) = jobs.recv().await {
        if job.done.is_closed() {
            // The caller stopped before any byte of this frame was written.
            continue;
        }
        let result = tokio::select! {
            biased;
            result = super::frame_writer::write_frame(&mut slot, job.line.as_bytes()) => result,
            // The caller stopped during the frame. Dropping the write future
            // drops stdin, as the frame writer requires.
            () = job.done.closed() => break,
        };
        let failed = result.is_err();
        let _ = job.done.send(result);
        if failed || slot.is_none() {
            break;
        }
    }
}

/// The pool-level route for wake requests of one worker.
#[derive(Clone)]
pub(crate) struct WakeRoute {
    tx: mpsc::Sender<WakeFrame>,
    worker: usize,
}

impl WakeRoute {
    /// Route for the worker at pool slot `worker`.
    pub(crate) fn new(tx: mpsc::Sender<WakeFrame>, worker: usize) -> Self {
        Self { tx, worker }
    }
}

/// Wake state of one child, shared by the client and its reader task.
pub(crate) struct WakeLink {
    /// Unique number of this child. A respawn makes a new child with a new
    /// number.
    generation: u64,
    /// The child advertised `buzz.wake` version 1 in its `initialize` result.
    supported: AtomicBool,
    /// The client was shut down or dropped. Its wake requests open no turn.
    retired: AtomicBool,
    /// Set once, when the pool takes the worker.
    route: OnceLock<WakeRoute>,
    /// Bounds the wake requests that wait for their response.
    in_flight: Arc<Semaphore>,
}

impl WakeLink {
    fn new() -> Self {
        Self {
            generation: NEXT_CHILD_GENERATION.fetch_add(1, Ordering::Relaxed),
            supported: AtomicBool::new(false),
            retired: AtomicBool::new(false),
            route: OnceLock::new(),
            in_flight: Arc::new(Semaphore::new(WAKES_IN_FLIGHT_PER_CHILD)),
        }
    }

    /// Unique number of this child.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// Record whether the child advertised `buzz.wake` version 1.
    pub(crate) fn set_supported(&self, supported: bool) {
        self.supported.store(supported, Ordering::Release);
    }

    /// Whether the child advertised `buzz.wake` version 1.
    pub(crate) fn supported(&self) -> bool {
        self.supported.load(Ordering::Acquire)
    }

    /// Mark the child as replaced. Its wake requests open no turn from now.
    pub(crate) fn retire(&self) {
        self.retired.store(true, Ordering::Release);
    }

    /// Attach the pool route. Returns `false` when a route was already set.
    pub(crate) fn attach(&self, route: WakeRoute) -> bool {
        self.route.set(route).is_ok()
    }

    /// Whether the pool route is set.
    pub(crate) fn has_route(&self) -> bool {
        self.route.get().is_some()
    }
}

/// Writes the one response of one wake request.
struct WakeResponder {
    id: serde_json::Value,
    writer: StdinWriter,
    permit: OwnedSemaphorePermit,
}

impl WakeResponder {
    fn respond(self, reply: wake::WakeReply) -> tokio::task::JoinHandle<()> {
        let line = wake::response_line(&self.id, &reply);
        let Self { id, writer, permit } = self;
        tokio::spawn(async move {
            // The permit is held until the response is written or fails.
            let _permit = permit;
            match tokio::time::timeout(WAKE_RESPONSE_TIMEOUT, writer.write_line(line)).await {
                Ok(Ok(())) => {
                    tracing::debug!(target: "acp::wake", "wake id={id} answered {reply:?}");
                }
                Ok(Err(error)) => {
                    tracing::warn!(target: "acp::wake", "wake id={id}: response not written: {error}");
                }
                Err(_) => {
                    tracing::warn!(
                        target: "acp::wake",
                        "wake id={id}: response not written within {WAKE_RESPONSE_TIMEOUT:?}"
                    );
                }
            }
        })
    }
}

/// A `_buzz/wake` request from one child, for the main loop.
///
/// It is answered at most once: [`respond`](Self::respond) consumes it. A
/// frame dropped without a response gets no answer; that is correct only for
/// a frame from a retired child.
pub(crate) struct WakeFrame {
    /// Pool slot of the worker whose child sent the request.
    pub(crate) worker: usize,
    /// Size of the request line in bytes, without the newline.
    pub(crate) frame_len: usize,
    /// The whole JSON-RPC request.
    pub(crate) message: serde_json::Value,
    link: Arc<WakeLink>,
    responder: WakeResponder,
}

impl WakeFrame {
    /// Generation of the child that sent the request.
    pub(crate) fn generation(&self) -> u64 {
        self.link.generation
    }

    /// Whether the child that sent the request is still the live child of
    /// its worker. A frame from a replaced child must open no turn.
    pub(crate) fn is_current(&self) -> bool {
        !self.link.retired.load(Ordering::Acquire)
    }

    /// Whether the child advertised `buzz.wake` version 1.
    pub(crate) fn child_supports_wake(&self) -> bool {
        self.link.supported()
    }

    /// The JSON-RPC id of the request, as received.
    pub(crate) fn id(&self) -> &serde_json::Value {
        &self.responder.id
    }

    /// Write the response to the child in a task and return that task.
    pub(crate) fn respond(self, reply: wake::WakeReply) -> tokio::task::JoinHandle<()> {
        self.responder.respond(reply)
    }
}

struct Reader {
    lines: mpsc::Sender<LineItem>,
    bytes: Arc<Semaphore>,
    state: Arc<ReadState>,
    link: Arc<WakeLink>,
    writer: StdinWriter,
    last_drop_warning: Option<Instant>,
}

impl Reader {
    /// Whether a line of `len` bytes would use the reserve of lines with an
    /// `id`.
    fn uses_idle_reserve(&self, len: usize) -> bool {
        self.lines.capacity() <= IDLE_RESERVE_LINES
            || self.bytes.available_permits() < len.saturating_add(IDLE_RESERVE_BYTES)
    }

    /// Give `item` to the read loop. Returns `false` when the client is gone.
    async fn deliver(&mut self, item: Result<String, LinesCodecError>) -> bool {
        let len = item.as_ref().map_or(0, String::len).min(LINE_QUEUE_BYTES);
        let permits = u32::try_from(len).unwrap_or(u32::MAX);
        let mut droppable: Option<bool> = None;
        loop {
            if self.state.is_idle()
                && self.uses_idle_reserve(len)
                && *droppable.get_or_insert_with(|| is_notification(&item))
            {
                self.record_drop();
                return true;
            }
            let lines = &self.lines;
            let bytes = Arc::clone(&self.bytes);
            tokio::select! {
                admitted = async {
                    let permit = bytes.acquire_many_owned(permits).await;
                    let slot = lines.reserve().await;
                    (permit, slot)
                } => {
                    return match admitted {
                        (Ok(permit), Ok(slot)) => {
                            slot.send(LineItem { line: item, _bytes: permit });
                            true
                        }
                        _ => false,
                    };
                }
                // The client went idle while this line waited: check again
                // whether it can be dropped.
                () = self.state.went_idle.notified() => {}
            }
        }
    }

    fn record_drop(&mut self) {
        let total = self.state.dropped.fetch_add(1, Ordering::Relaxed) + 1;
        let now = Instant::now();
        let due = self
            .last_drop_warning
            .is_none_or(|last| now.duration_since(last) >= DROP_WARN_INTERVAL);
        if due {
            self.last_drop_warning = Some(now);
            tracing::warn!(
                target: "acp::wire",
                dropped_total = total,
                generation = self.link.generation,
                "agent writes notifications while idle faster than they are read — dropping them"
            );
        }
    }

    /// Send a wake request to the pool, or answer `busy` when the pool cannot
    /// take it now. Waits only while this child has the maximum number of
    /// unanswered wake requests.
    async fn route_wake(&self, frame_len: usize, message: serde_json::Value) {
        let id = message
            .get("id")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let Ok(permit) = Arc::clone(&self.link.in_flight).acquire_owned().await else {
            return; // The semaphore is never closed.
        };
        let responder = WakeResponder {
            id,
            writer: self.writer.clone(),
            permit,
        };
        let Some(route) = self.link.route.get() else {
            tracing::info!(
                target: "acp::wake",
                generation = self.link.generation,
                "wake request before the pool took this agent — answering busy"
            );
            drop(responder.respond(Ok(wake::WakeOutcome::Busy)));
            return;
        };
        let frame = WakeFrame {
            worker: route.worker,
            frame_len,
            message,
            link: Arc::clone(&self.link),
            responder,
        };
        if let Err(error) = route.tx.try_send(frame) {
            let frame = match error {
                mpsc::error::TrySendError::Full(frame)
                | mpsc::error::TrySendError::Closed(frame) => frame,
            };
            tracing::warn!(
                target: "acp::wake",
                worker = frame.worker,
                "wake queue of the main loop is full or closed — answering busy"
            );
            drop(frame.respond(Ok(wake::WakeOutcome::Busy)));
        }
    }
}

/// A line is a notification when it has no `id`. A line that is not JSON
/// carries no request or response either.
fn is_notification(item: &Result<String, LinesCodecError>) -> bool {
    let Ok(line) = item else {
        return false;
    };
    match serde_json::from_str::<serde_json::Value>(line) {
        Ok(message) => message.get("id").is_none(),
        Err(_) => true,
    }
}

async fn run_reader<R: AsyncRead + Unpin>(stdout: R, mut reader: Reader) {
    let mut framed = FramedRead::new(
        stdout,
        LinesCodec::new_with_max_length(super::MAX_LINE_SIZE),
    );
    while let Some(item) = framed.next().await {
        if let Ok(line) = &item {
            if let Some(message) = wake::parse_wake_request(line) {
                reader.route_wake(line.len(), message).await;
                continue;
            }
        }
        if !reader.deliver(item).await {
            break;
        }
    }
}

/// Ends the reader and writer tasks and retires the child when the client
/// drops.
pub(crate) struct ChildIoGuard {
    reader: tokio::task::JoinHandle<()>,
    writer: tokio::task::JoinHandle<()>,
    link: Arc<WakeLink>,
}

impl Drop for ChildIoGuard {
    fn drop(&mut self) {
        self.link.retire();
        self.reader.abort();
        self.writer.abort();
    }
}

/// The stdio handles of one child after its tasks started.
pub(crate) struct ChildIo {
    pub(crate) lines: LineReceiver,
    pub(crate) writer: StdinWriter,
    pub(crate) link: Arc<WakeLink>,
    pub(crate) guard: ChildIoGuard,
}

/// Start the reader task on `stdout` and the writer task on `stdin`.
pub(crate) fn start<R, W>(stdout: R, stdin: W) -> ChildIo
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (write_tx, write_rx) = mpsc::channel(WRITE_QUEUE_FRAMES);
    let writer = StdinWriter { tx: write_tx };
    let (line_tx, line_rx) = mpsc::channel(LINE_QUEUE_LINES);
    let state = Arc::new(ReadState::default());
    let link = Arc::new(WakeLink::new());
    let reader = Reader {
        lines: line_tx,
        bytes: Arc::new(Semaphore::new(LINE_QUEUE_BYTES)),
        state: Arc::clone(&state),
        link: Arc::clone(&link),
        writer: writer.clone(),
        last_drop_warning: None,
    };
    let reader_task = tokio::spawn(run_reader(stdout, reader));
    let writer_task = tokio::spawn(run_writer(stdin, write_rx));
    ChildIo {
        lines: LineReceiver { rx: line_rx, state },
        writer,
        link: Arc::clone(&link),
        guard: ChildIoGuard {
            reader: reader_task,
            writer: writer_task,
            link,
        },
    }
}

#[cfg(test)]
#[path = "child_io_tests.rs"]
mod tests;
