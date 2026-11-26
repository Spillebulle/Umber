//! Talking to a cutter: the conversation a job is, and the link it travels on.
//!
//! `umber_core::cutjob` builds the bytes; this module decides when each block
//! is written and what the machine has to say before the next one goes. It is
//! in `umber-app` because the link is the platform's, and it holds no egui and
//! no winit: the [`Worker`] here runs a job on its own thread and wakes the
//! loop through a plain closure, and the dialog that watches it is
//! `cutterdlg`.
//!
//! **Nobody has run this against a machine.** Everything below is pinned
//! against [`Link`] implementations the tests script themselves, which proves
//! what Umber writes and how it reads what comes back, and proves nothing about
//! what a Silhouette actually answers.
//!
//! Three rules the conversation keeps, each of which is a cut in the wrong
//! place if it is broken:
//!
//! - **Nothing is ever written twice.** A write that failed, or that may have
//!   reached the machine only in part, ends the job. Resending a block the
//!   machine already holds half of is the blade going round a second time from
//!   wherever the first copy stopped.
//! - **An answer Umber does not recognise is never taken for ready.** A status
//!   is exactly one of three byte strings; anything else, a `3` or a `4`
//!   included, ends the job and is reported as it arrived.
//! - **The marks reply is compared byte for byte, spaces and all.** The machine
//!   pads its number; `"    0"` is found and `"0"` is not something it says.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use umber_core::cutjob::{ETX, Job};

/// Silhouette's USB vendor id.
pub const VENDOR: u16 = 0x0b4d;
/// The Portrait 3's product id.
pub const PORTRAIT_3: u16 = 0x113a;

/// Initialise: the machine forgets any half-received command.
const INIT: &[u8] = b"\x1b\x04";
/// Ask for the firmware version.
const FIRMWARE: &[u8] = b"FG\x03";
/// Ask for the status.
const STATUS: &[u8] = b"\x1b\x05";
/// The one reply that means the marks were found.
const MARKS_FOUND: &[u8] = b"    0\x03";

/// Why the link could not carry a write or a reply.
#[derive(Debug)]
pub enum LinkError {
    /// No machine of that kind is plugged in.
    NotFound,
    /// Another program holds the machine.
    Busy,
    /// Nothing arrived in time.
    Timeout,
    /// The operating system refused, or a write went only part of the way.
    Io(String),
    /// This platform has no link.
    Unsupported,
}

/// The wire to the machine.
///
/// A reply is everything up to and including one ETX; whatever arrived after
/// it belongs to the next reply.
pub trait Link {
    /// Write all of `bytes`, or fail. A failure may have delivered part of
    /// them, so a caller never retries one.
    fn write(&mut self, bytes: &[u8]) -> Result<(), LinkError>;
    /// The next reply, ETX included.
    fn read_reply(&mut self, timeout: Duration) -> Result<Vec<u8>, LinkError>;
    /// Throw away everything that has arrived, and keep reading and throwing
    /// away until `quiet` has passed with nothing new (bounded overall at
    /// [`DISCARD_LIMIT`]). Answers what was thrown away. Nothing arriving is
    /// not an error here; the operating system refusing is.
    ///
    /// Called before every status query, because anything already queued then
    /// was sent before the question and cannot be its answer.
    fn discard(&mut self, quiet: Duration) -> Result<Vec<u8>, LinkError>;
}

/// The longest one [`Link::discard`] goes on reading, however much keeps
/// arriving.
pub const DISCARD_LIMIT: Duration = Duration::from_secs(2);

/// What the machine says when asked how it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Ready,
    Moving,
    NoMedia,
}

/// A status reply, or the reply itself when it is not one of the three.
pub fn status_of(reply: &[u8]) -> Result<Status, Vec<u8>> {
    match reply {
        b"0\x03" => Ok(Status::Ready),
        b"1\x03" => Ok(Status::Moving),
        b"2\x03" => Ok(Status::NoMedia),
        other => Err(other.to_vec()),
    }
}

/// Whether the reply to the marks search says they were found.
pub fn marks_found(reply: &[u8]) -> bool {
    reply == MARKS_FOUND
}

/// How far a job has got, reported as each part begins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Connecting,
    FindingMarks,
    /// `block` counts from zero.
    Cutting {
        block: usize,
        of: usize,
    },
    Finishing,
    /// A stop was seen: nothing more is cut, and the machine is waited for
    /// and sent home.
    Stopping,
}

/// Why a job ended before the machine was sent home.
#[derive(Debug)]
pub enum SendError {
    Link(LinkError),
    /// No mat or sheet is loaded.
    NoMedia,
    /// The marks search ended in anything but the found reply, which is kept.
    MarksNotFound(Vec<u8>),
    /// The marks search got no reply at all within [`Timing::marks`].
    MarksTimedOut,
    /// A status Umber does not recognise, as it arrived.
    Unknown(Vec<u8>),
    /// The machine stayed busy past [`Timing::ready`].
    NeverReady,
    /// The stop flag was raised, and the machine finished what it held and
    /// was sent home.
    Stopped,
    /// Everything was written, the end block included, and then the last wait
    /// did not end in Ready. The raw reply where there was one that was not a
    /// status, and empty otherwise.
    Unconfirmed(Vec<u8>),
}

/// Why a job ended early, and how far it had got: an artist who is told only
/// that a job failed, and sends it again, cuts the sheet twice.
#[derive(Debug)]
pub struct Failure {
    pub error: SendError,
    /// Path blocks written, counting a block whose write failed or timed out,
    /// because it may have reached the machine in part.
    pub blocks_sent: usize,
    /// Path blocks in the whole job.
    pub blocks: usize,
}

impl Failure {
    /// What to tell the artist: the error's own sentence, and, where any of
    /// the sheet may already be cut, that it should be looked at first.
    pub fn sentence(&self) -> String {
        let mut s = self.error.sentence();
        // Exhaustive on purpose: a new variant has to say whether its own
        // sentence already tells the artist the state of the sheet.
        let says_so = match self.error {
            SendError::Unconfirmed(_) => true,
            SendError::Link(_)
            | SendError::NoMedia
            | SendError::MarksNotFound(_)
            | SendError::MarksTimedOut
            | SendError::Unknown(_)
            | SendError::NeverReady
            | SendError::Stopped => false,
        };
        if self.blocks_sent > 0 && !says_so {
            s.push_str(" Part of the sheet may already be cut. Check it before sending again.");
        }
        s
    }
}

impl From<LinkError> for SendError {
    fn from(e: LinkError) -> Self {
        SendError::Link(e)
    }
}

impl SendError {
    /// What to tell the artist.
    pub fn sentence(&self) -> String {
        match self {
            SendError::Link(e) => match e {
                LinkError::NotFound => "No cutter was found. Check that it is plugged in and \
                                        switched on."
                    .to_string(),
                LinkError::Busy => "The cutter is in use by another program. Close Silhouette \
                                    Studio and try again."
                    .to_string(),
                LinkError::Timeout => "The cutter did not respond in time.".to_string(),
                LinkError::Io(why) => format!("The connection to the cutter failed: {why}."),
                LinkError::Unsupported => {
                    "Sending to a cutter works on Windows only for now.".to_string()
                }
            },
            SendError::NoMedia => {
                "There is no mat or sheet in the cutter. Load one and try again.".to_string()
            }
            SendError::MarksNotFound(raw) => format!(
                "The cutter did not report finding the registration marks, so nothing was \
                 cut. It answered: {}.",
                escaped(raw)
            ),
            SendError::MarksTimedOut => "The cutter did not answer while looking for the \
                                         registration marks. Nothing was cut."
                .to_string(),
            SendError::Unknown(raw) => format!(
                "The cutter gave an answer Umber does not understand ({}), so nothing more was \
                 sent.",
                escaped(raw)
            ),
            SendError::NeverReady => "The cutter stayed busy and never became ready.".to_string(),
            SendError::Stopped => "Stopped. The cutter finished the part it had already been \
                                   sent, and nothing more was sent."
                .to_string(),
            SendError::Unconfirmed(_) => "The whole job was sent, but the cutter did not \
                                          confirm it had finished. Check the sheet before \
                                          sending again."
                .to_string(),
        }
    }
}

/// How long the conversation waits for each thing.
#[derive(Clone, Copy, Debug)]
pub struct Timing {
    /// Between status queries while the machine is moving.
    pub poll: Duration,
    /// How long the machine may stay busy before the job gives up.
    pub ready: Duration,
    /// How long the marks search may take to answer.
    pub marks: Duration,
    /// How long any other reply may take.
    pub reply: Duration,
    /// The quiet that ends a discard after a command that may answer more
    /// than once: the initialise, and a marks search that succeeded.
    pub settle: Duration,
    /// The quiet that ends the discard before every status query.
    pub drain: Duration,
}

impl Timing {
    pub const REAL: Timing = Timing {
        poll: Duration::from_millis(250),
        // One block of a kilobyte can take many minutes to cut, and a machine
        // that keeps answering Moving has shown it is alive. A machine that
        // stops answering at all is a `Timeout` after `reply`, not this.
        ready: Duration::from_secs(45 * 60),
        marks: Duration::from_secs(40),
        reply: Duration::from_secs(5),
        settle: Duration::from_millis(500),
        drain: Duration::from_millis(30),
    };
}

/// Send a whole job. `Ok` carries the firmware string the machine gave; a
/// [`Failure`] says how many path blocks had been written when it ended.
///
/// `stop` is read before the setup, the search and each path block, and after
/// every Moving while the machine is being waited for, which is when
/// [`Stage::Stopping`] is said.
///
/// **A stop does not end the job at once, because the machine has not
/// stopped.** It holds up to a kilobyte of moves when the stop is pressed and
/// goes on cutting them, so ending there would tell the artist it had
/// finished while it had not, and would leave the tool wherever the last move
/// put it, which may be down on the sheet. So once the stop is seen nothing is
/// written but status queries and the end block: the machine is waited for,
/// as it is between blocks, until it says Ready; then the end block, which is
/// a move with the tool up to the origin, goes once; then the machine is
/// waited for again, and only then is the job [`SendError::Stopped`]. A stop
/// seen before any path block went still sends the machine home, which costs
/// a move and nothing else.
///
/// **After an error part way through, the end block is deliberately not
/// sent**, stop or no stop. The link may be what failed, and the end block is
/// a move like any other, so sending it would be writing into a conversation
/// Umber has already lost track of. What that leaves the machine doing is not
/// known, and has to be checked on a real Portrait 3.
///
/// A failure in the very last wait of a job that was not stopped, after the
/// end block went, is [`SendError::Unconfirmed`]: the whole job is on the
/// machine, and reading that as a failed job is how a sheet gets cut twice.
/// A stop raised during that wait changes nothing; the job is all sent.
pub fn send(
    link: &mut dyn Link,
    job: &Job,
    timing: &Timing,
    stop: &AtomicBool,
    progress: &mut dyn FnMut(Stage),
) -> Result<String, Failure> {
    let blocks = job.path_blocks();
    let mut sent = 0;
    conversation(link, job, &blocks, timing, stop, progress, &mut sent).map_err(|error| Failure {
        error,
        blocks_sent: sent,
        blocks: blocks.len(),
    })
}

/// The whole of [`send`], counting into `sent` each path block as its write
/// begins.
fn conversation(
    link: &mut dyn Link,
    job: &Job,
    blocks: &[Vec<u8>],
    timing: &Timing,
    stop: &AtomicBool,
    progress: &mut dyn FnMut(Stage),
    sent: &mut usize,
) -> Result<String, SendError> {
    let mut stop = Stopper {
        flag: stop,
        said: false,
    };
    progress(Stage::Connecting);
    let firmware = hello(link, timing)?;
    wait_ready(link, timing, &mut stop, progress)?;
    if stop.seen(progress) {
        return go_home(link, job, timing, progress);
    }
    link.write(&job.setup_block())?;
    wait_ready(link, timing, &mut stop, progress)?;

    if let Some(marks) = job.marks_block() {
        if stop.seen(progress) {
            return go_home(link, job, timing, progress);
        }
        progress(Stage::FindingMarks);
        link.write(&marks)?;
        // The first reply is the verdict. A search that succeeded answers
        // more than once (three times, the reference implementation says),
        // and the rest are settled away rather than left to be read as the
        // status of the first block.
        let reply = match link.read_reply(timing.marks) {
            Ok(reply) => reply,
            Err(LinkError::Timeout) => return Err(SendError::MarksTimedOut),
            Err(e) => return Err(e.into()),
        };
        if !marks_found(&reply) {
            return Err(SendError::MarksNotFound(reply));
        }
        link.discard(timing.settle)?;
        // The search moved the carriage; nothing is cut until it is done.
        wait_ready(link, timing, &mut stop, progress)?;
    }

    let of = blocks.len();
    for (block, bytes) in blocks.iter().enumerate() {
        if stop.seen(progress) {
            return go_home(link, job, timing, progress);
        }
        progress(Stage::Cutting { block, of });
        // Counted before the write: one that fails may have gone in part.
        *sent += 1;
        link.write(bytes)?;
        wait_ready(link, timing, &mut stop, progress)?;
    }

    if stop.seen(progress) {
        return go_home(link, job, timing, progress);
    }
    progress(Stage::Finishing);
    link.write(&job.end_block())?;
    // Everything is on the machine now, so however this wait ends it is not
    // a failed job, and a stop raised now changes nothing the machine does.
    // It is not watched for here, so it is not said either.
    let mut ignored = Stopper {
        flag: &AtomicBool::new(false),
        said: true,
    };
    wait_ready(link, timing, &mut ignored, progress).map_err(|e| {
        SendError::Unconfirmed(match e {
            SendError::Unknown(raw) => raw,
            SendError::Link(_)
            | SendError::NoMedia
            | SendError::MarksNotFound(_)
            | SendError::MarksTimedOut
            | SendError::NeverReady
            | SendError::Stopped
            | SendError::Unconfirmed(_) => Vec::new(),
        })
    })?;
    Ok(firmware)
}

/// Whether the stop has been raised, said once as [`Stage::Stopping`] the
/// first time it is seen.
struct Stopper<'a> {
    flag: &'a AtomicBool,
    said: bool,
}

impl Stopper<'_> {
    fn seen(&mut self, progress: &mut dyn FnMut(Stage)) -> bool {
        if self.said {
            return true;
        }
        if self.flag.load(Ordering::SeqCst) {
            self.said = true;
            progress(Stage::Stopping);
            return true;
        }
        false
    }
}

/// The end of a stopped job: let the machine finish what it holds, send it
/// home with the tool up, and wait for that too. Any failure on the way is
/// that failure, and is reported in place of the stop.
fn go_home(
    link: &mut dyn Link,
    job: &Job,
    timing: &Timing,
    progress: &mut dyn FnMut(Stage),
) -> Result<String, SendError> {
    // Already stopped and already said, so the waits watch for nothing.
    let mut stopped = Stopper {
        flag: &AtomicBool::new(true),
        said: true,
    };
    wait_ready(link, timing, &mut stopped, progress)?;
    link.write(&job.end_block())?;
    wait_ready(link, timing, &mut stopped, progress)?;
    Err(SendError::Stopped)
}

/// The three harmless commands and nothing else: initialise, the firmware
/// string and one status. Nothing moves.
pub fn probe(
    link: &mut dyn Link,
    timing: &Timing,
) -> Result<(String, Result<Status, Vec<u8>>), LinkError> {
    let firmware = hello(link, timing)?;
    let reply = ask_status(link, timing)?;
    Ok((firmware, status_of(&reply)))
}

/// Initialise and ask for the firmware string: what comes before the ETX, as
/// text, trimmed. Whatever the initialise answers, if it answers at all, is
/// settled away first, so it cannot be taken for the firmware.
fn hello(link: &mut dyn Link, timing: &Timing) -> Result<String, LinkError> {
    link.write(INIT)?;
    link.discard(timing.settle)?;
    link.write(FIRMWARE)?;
    let reply = link.read_reply(timing.reply)?;
    let text = reply.split(|&b| b == ETX).next().unwrap_or(&[]);
    Ok(String::from_utf8_lossy(text).trim().to_string())
}

/// One status query and its reply. Anything queued before the query is
/// written was sent before the question and cannot be its answer, so it is
/// drained first; that is what keeps a leftover from putting every later poll
/// one answer behind.
fn ask_status(link: &mut dyn Link, timing: &Timing) -> Result<Vec<u8>, LinkError> {
    link.discard(timing.drain)?;
    link.write(STATUS)?;
    link.read_reply(timing.reply)
}

/// Ask until the machine is ready. Moving is asked again after
/// [`Timing::poll`] until [`Timing::ready`] has passed. The stop is looked at
/// after every Moving, so that it is said while a block is still cutting, and
/// the wait goes on regardless: a stopped machine is still a moving one. A
/// query that gets no reply at all is a timeout at once, and anything that is
/// not a status ends the job rather than being asked again.
fn wait_ready(
    link: &mut dyn Link,
    timing: &Timing,
    stop: &mut Stopper,
    progress: &mut dyn FnMut(Stage),
) -> Result<(), SendError> {
    let started = Instant::now();
    loop {
        let reply = ask_status(link, timing)?;
        match status_of(&reply) {
            Ok(Status::Ready) => return Ok(()),
            Ok(Status::NoMedia) => return Err(SendError::NoMedia),
            Ok(Status::Moving) => {
                stop.seen(progress);
                if started.elapsed() >= timing.ready {
                    return Err(SendError::NeverReady);
                }
                std::thread::sleep(timing.poll);
            }
            Err(raw) => return Err(SendError::Unknown(raw)),
        }
    }
}

/// Bytes as a line of text: printable ASCII as itself, a backslash doubled,
/// everything else as `\xNN`.
pub fn escaped(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        match b {
            b'\\' => out.push_str("\\\\"),
            0x20..=0x7e => out.push(char::from(b)),
            _ => out.push_str(&format!("\\x{b:02x}")),
        }
    }
    out
}

/// One log line: direction, count, and the bytes, a long run cut short.
fn log_line(direction: char, bytes: &[u8]) -> String {
    if bytes.len() > LOG_BYTES {
        format!(
            "{direction} {} {} ... ({} bytes)",
            bytes.len(),
            escaped(&bytes[..LOG_BYTES]),
            bytes.len()
        )
    } else {
        format!("{direction} {} {}", bytes.len(), escaped(bytes))
    }
}

/// The most bytes of one write a log line shows.
const LOG_BYTES: usize = 96;

/// A link that hands one line per transfer to a sink, and otherwise is the
/// link it wraps.
pub struct Logged<L: Link, F: FnMut(String)> {
    link: L,
    sink: F,
}

impl<L: Link, F: FnMut(String)> Logged<L, F> {
    pub fn new(link: L, sink: F) -> Self {
        Logged { link, sink }
    }
}

impl<L: Link, F: FnMut(String)> Link for Logged<L, F> {
    fn write(&mut self, bytes: &[u8]) -> Result<(), LinkError> {
        let out = self.link.write(bytes);
        let line = log_line('>', bytes);
        match &out {
            Ok(()) => (self.sink)(line),
            Err(e) => (self.sink)(format!("{line} failed: {e:?}")),
        }
        out
    }

    fn read_reply(&mut self, timeout: Duration) -> Result<Vec<u8>, LinkError> {
        let out = self.link.read_reply(timeout);
        match &out {
            Ok(reply) => (self.sink)(log_line('<', reply)),
            Err(e) => (self.sink)(format!("< failed: {e:?}")),
        }
        out
    }

    /// Bytes thrown away are logged as one line with direction `x`, and a
    /// discard that found nothing is not logged at all, so a status poll costs
    /// the log no more than it did.
    fn discard(&mut self, quiet: Duration) -> Result<Vec<u8>, LinkError> {
        let out = self.link.discard(quiet);
        match &out {
            Ok(bytes) if bytes.is_empty() => {}
            Ok(bytes) => (self.sink)(log_line('x', bytes)),
            Err(e) => (self.sink)(format!("x failed: {e:?}")),
        }
        out
    }
}

/// A boxed link is the link it holds, which is what lets the worker wrap
/// whatever its opener produced in [`Logged`].
impl<L: Link + ?Sized> Link for Box<L> {
    fn write(&mut self, bytes: &[u8]) -> Result<(), LinkError> {
        (**self).write(bytes)
    }
    fn read_reply(&mut self, timeout: Duration) -> Result<Vec<u8>, LinkError> {
        (**self).read_reply(timeout)
    }
    fn discard(&mut self, quiet: Duration) -> Result<Vec<u8>, LinkError> {
        (**self).discard(quiet)
    }
}

// ------------------------------------------------------------------ the worker

/// What a worker says, in the order it happens.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// One line for the log: a transfer, or something the opener found.
    Line(String),
    /// A job has reached a new part.
    Stage(Stage),
    /// A connection check ended: the sentence to show, or why it failed.
    Checked(Result<String, String>),
    /// A job ended: the sentence to show, or why it failed.
    Sent(Result<String, String>),
}

impl Event {
    /// Whether nothing follows this one.
    fn is_final(&self) -> bool {
        match self {
            Event::Line(_) | Event::Stage(_) => false,
            Event::Checked(_) | Event::Sent(_) => true,
        }
    }
}

/// One piece of work for a worker.
pub enum Work {
    /// Ask the machine who it is and how it is. Nothing moves.
    Check,
    /// Send this job. The worker owns it, so nothing it does afterwards reads
    /// the document, which may have been closed or switched by then.
    Send(Job),
}

/// How a worker reaches the machine. It is handed a sink for log lines, so
/// the real opener can say which machines it found before it opens one, and a
/// test's opener says nothing platform specific.
pub type Opener =
    Box<dyn FnOnce(&mut dyn FnMut(String)) -> Result<Box<dyn Link + Send>, LinkError> + Send>;

/// What the real opener logs before it opens: every Silhouette interface it
/// found, or that it found none.
pub fn found_lines(paths: &[String]) -> Vec<String> {
    if paths.is_empty() {
        return vec!["No Silhouette cutter found on USB.".to_string()];
    }
    paths
        .iter()
        .map(|p| format!("Found a Silhouette at {p}"))
        .collect()
}

/// The sentence a worker that died before answering is reported with.
const SEND_DIED: &str =
    "The connection to the cutter stopped unexpectedly. Check the sheet before sending again.";
const CHECK_DIED: &str = "The connection to the cutter stopped unexpectedly.";

/// The worker's end of the channel, and the wake that must follow every
/// event and the end of the thread.
///
/// **It owns the sender, and the wake in `drop` comes after the sender has
/// gone.** On a panic nothing final was sent, so the frame the wake asks for
/// has to find the channel disconnected; waking first would let it read
/// `Empty` and sleep with no second wake coming. `loading::Finish` is the same
/// shape for the same reason, and CLAUDE.md's "try_recv has three readings"
/// is the bug it closes. Everything that logs or reports progress borrows
/// this rather than cloning the sender, so there is exactly one sender and the
/// borrow checker drops everything that borrowed it first.
struct Finish {
    /// `Option` only so `drop` can let it go before the wake.
    tx: Option<std::sync::mpsc::Sender<Event>>,
    wake: Box<dyn Fn() + Send>,
}

impl Finish {
    fn send(&self, event: Event) {
        if let Some(tx) = self.tx.as_ref() {
            // Failing means the worker handle was dropped, which is nobody
            // left to tell.
            let _ = tx.send(event);
        }
        (self.wake)();
    }
}

impl Drop for Finish {
    fn drop(&mut self) {
        drop(self.tx.take());
        // Nothing here may panic: a panic in a destructor while unwinding
        // aborts the process.
        (self.wake)();
    }
}

/// A connection check or a job running on its own thread.
pub struct Worker {
    rx: std::sync::mpsc::Receiver<Event>,
    stop: std::sync::Arc<AtomicBool>,
    /// Which final event a death is reported as.
    checking: bool,
    /// A final event has been handed out, so nothing more will be.
    done: bool,
}

impl Worker {
    /// Start `work` on a thread of its own. `wake` is called after every event
    /// and once more when the thread ends, however it ends.
    pub fn start(
        work: Work,
        open: Opener,
        timing: Timing,
        wake: impl Fn() + Send + 'static,
    ) -> Worker {
        let (tx, rx) = std::sync::mpsc::channel();
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let checking = match &work {
            Work::Check => true,
            Work::Send(_) => false,
        };
        let finish = Finish {
            tx: Some(tx),
            wake: Box::new(wake),
        };
        let flag = std::sync::Arc::clone(&stop);
        let started = std::thread::Builder::new()
            .name("umber-cutter".to_owned())
            .spawn(move || {
                let finish = finish;
                run(work, open, &timing, &flag, &finish);
            });
        if let Err(e) = started {
            // The closure went with the failure and took `Finish` with it, so
            // the channel is already disconnected and `poll` reports the death.
            log::error!("could not start the cutter worker: {e}");
        }
        Worker {
            rx,
            stop,
            checking,
            done: false,
        }
    }

    /// Ask the job to stop. The worker answers, with the final event, once it
    /// has: the conversation reads the flag before each block and inside each
    /// wait, and never abandons a write half way.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    /// Everything said since the last poll. A worker that ended without a
    /// final event yields one error event in its place, once, and after the
    /// final event nothing more is ever handed out.
    pub fn poll(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        while !self.done {
            match self.rx.try_recv() {
                Ok(event) => {
                    self.done = event.is_final();
                    out.push(event);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                // Every sender gone without a final event: the thread died.
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.done = true;
                    out.push(if self.checking {
                        Event::Checked(Err(CHECK_DIED.to_string()))
                    } else {
                        Event::Sent(Err(SEND_DIED.to_string()))
                    });
                }
            }
        }
        out
    }

    /// Whether the final event has been handed out.
    pub fn finished(&self) -> bool {
        self.done
    }
}

/// The worker's whole body. The link is dropped, closing the handle, before
/// the final event is sent, so a second job started on that event cannot find
/// the machine still held by this one.
fn run(work: Work, open: Opener, timing: &Timing, stop: &AtomicBool, finish: &Finish) {
    let opened = open(&mut |line| finish.send(Event::Line(line)));
    match work {
        Work::Check => {
            let out = opened.and_then(|link| {
                let mut link = Logged::new(link, |line| finish.send(Event::Line(line)));
                probe(&mut link, timing)
            });
            finish.send(Event::Checked(match out {
                Ok((firmware, status)) => Ok(checked(&firmware, status)),
                Err(e) => Err(SendError::Link(e).sentence()),
            }));
        }
        Work::Send(job) => {
            let out = match opened {
                Err(e) => Err(Failure {
                    error: SendError::Link(e),
                    blocks_sent: 0,
                    blocks: job.path_blocks().len(),
                }),
                Ok(link) => {
                    let mut link = Logged::new(link, |line| finish.send(Event::Line(line)));
                    send(&mut link, &job, timing, stop, &mut |stage| {
                        finish.send(Event::Stage(stage))
                    })
                }
            };
            finish.send(Event::Sent(match out {
                Ok(_) => Ok("Sent.".to_string()),
                Err(failure) => Err(failure.sentence()),
            }));
        }
    }
}

/// What a connection check found, as one sentence. An answer Umber does not
/// recognise is said as it arrived, and is never called ready.
fn checked(firmware: &str, status: Result<Status, Vec<u8>>) -> String {
    let who = if firmware.is_empty() {
        "Connected. The cutter gave no firmware string.".to_string()
    } else {
        format!("Connected. Firmware {firmware}.")
    };
    let how = match status {
        Ok(Status::Ready) => "The cutter is ready.".to_string(),
        Ok(Status::Moving) => "The cutter is busy.".to_string(),
        Ok(Status::NoMedia) => "No sheet is loaded.".to_string(),
        Err(raw) => format!(
            "The cutter answered with a status Umber does not know: {}.",
            escaped(&raw)
        ),
    };
    format!("{who} {how}")
}

/// Whether a device interface path names a Silhouette, and the product
/// asked for where one is.
///
/// A path reads `\\?\usb#vid_0b4d&pid_113a#<instance>#{<interface>}`, so the
/// ids are the `&`-separated tokens of the second `#` field and nowhere else:
/// an instance string is the device's own and may hold anything.
#[cfg(any(windows, test))]
fn is_cutter(path: &str, product: Option<u16>) -> bool {
    let path = path.to_ascii_lowercase();
    let Some(ids) = path.split('#').nth(1) else {
        return false;
    };
    let has = |token: String| ids.split('&').any(|t| t == token);
    has(format!("vid_{VENDOR:04x}"))
        && match product {
            Some(pid) => has(format!("pid_{pid:04x}")),
            None => true,
        }
}

/// The first whole reply out of what has arrived, ETX included. What follows
/// the ETX stays for the next reply.
#[cfg(any(windows, test))]
fn take_reply(pending: &mut Vec<u8>) -> Option<Vec<u8>> {
    let end = pending.iter().position(|&b| b == ETX)?;
    Some(pending.drain(..=end).collect())
}

/// The machine on Windows' USB printing class driver, which is what a
/// Silhouette enumerates as.
///
/// **Nobody has run this against a machine.** It is written to what the
/// documentation says each call does, and every signature was read from the
/// `windows-sys` source rather than remembered.
///
/// **A known unknown: a cancel the driver never completes.** A read or a write
/// that outlives its wait is cancelled with `CancelIoEx` and then waited for
/// with `GetOverlappedResult(…, TRUE)`, which does not return until the
/// operation has finished. A driver that never completes a cancelled request
/// would therefore block the worker thread for good. There is no sound
/// alternative: the buffer and the OVERLAPPED belong to the operation until it
/// completes, so returning early would free memory the kernel may still write
/// to. A bounded wait that then leaked both would be sound and would leave the
/// handle unusable, which is no better for the artist. Whether the USB
/// printing driver honours a cancel promptly is one of the things to find out
/// on the machine.
pub struct UsbPrint {
    #[cfg(windows)]
    handle: win::Handle,
    /// What has arrived and not yet been handed out as a reply.
    #[cfg(windows)]
    pending: Vec<u8>,
    /// There is no link off Windows, so no value of this type exists there.
    #[cfg(not(windows))]
    _none: std::convert::Infallible,
}

impl UsbPrint {
    /// Open the first machine of `product` that is plugged in.
    pub fn open(product: u16) -> Result<UsbPrint, LinkError> {
        #[cfg(windows)]
        {
            let path = win::interface_paths()
                .into_iter()
                .find(|p| is_cutter(p, Some(product)))
                .ok_or(LinkError::NotFound)?;
            Ok(UsbPrint {
                handle: win::open(&path)?,
                pending: Vec::new(),
            })
        }
        #[cfg(not(windows))]
        {
            let _ = product;
            Err(LinkError::Unsupported)
        }
    }

    /// Every Silhouette interface path that is plugged in, whatever the model.
    pub fn paths() -> Vec<String> {
        #[cfg(windows)]
        {
            win::interface_paths()
                .into_iter()
                .filter(|p| is_cutter(p, None))
                .collect()
        }
        #[cfg(not(windows))]
        {
            Vec::new()
        }
    }
}

impl Link for UsbPrint {
    fn write(&mut self, bytes: &[u8]) -> Result<(), LinkError> {
        #[cfg(windows)]
        {
            win::write(&self.handle, bytes)
        }
        #[cfg(not(windows))]
        {
            let _ = bytes;
            Err(LinkError::Unsupported)
        }
    }

    fn read_reply(&mut self, timeout: Duration) -> Result<Vec<u8>, LinkError> {
        #[cfg(windows)]
        {
            let deadline = Instant::now() + timeout;
            loop {
                if let Some(reply) = take_reply(&mut self.pending) {
                    return Ok(reply);
                }
                let now = Instant::now();
                if now >= deadline {
                    // Whatever part of a reply did arrive stays in `pending`.
                    return Err(LinkError::Timeout);
                }
                let mut buf = [0u8; win::READ_BYTES];
                let got = win::read(&self.handle, &mut buf, deadline - now)?;
                self.pending.extend_from_slice(&buf[..got]);
                if got == 0 {
                    // A read that completes empty is not a reason to spin.
                    let left = deadline.saturating_duration_since(Instant::now());
                    std::thread::sleep(Duration::from_millis(10).min(left));
                }
            }
        }
        #[cfg(not(windows))]
        {
            let _ = timeout;
            Err(LinkError::Unsupported)
        }
    }

    fn discard(&mut self, quiet: Duration) -> Result<Vec<u8>, LinkError> {
        #[cfg(windows)]
        {
            let mut thrown = std::mem::take(&mut self.pending);
            let limit = Instant::now() + DISCARD_LIMIT;
            let mut last = Instant::now();
            loop {
                let now = Instant::now();
                let quiet_left = quiet.saturating_sub(now.duration_since(last));
                if quiet_left.is_zero() || now >= limit {
                    return Ok(thrown);
                }
                let wait = quiet_left.min(limit - now);
                let mut buf = [0u8; win::READ_BYTES];
                // A read that times out is cancelled and waited for inside
                // `win::read`, exactly as for a reply, and answers what arrived.
                let got = win::read(&self.handle, &mut buf, wait)?;
                if got > 0 {
                    thrown.extend_from_slice(&buf[..got]);
                    last = Instant::now();
                } else {
                    let left = limit.saturating_duration_since(Instant::now());
                    std::thread::sleep(Duration::from_millis(5).min(left).min(wait));
                }
            }
        }
        #[cfg(not(windows))]
        {
            let _ = quiet;
            Err(LinkError::Unsupported)
        }
    }
}

#[cfg(windows)]
impl Drop for UsbPrint {
    fn drop(&mut self) {
        win::cancel_all(&self.handle);
        // The handle itself is closed by its own drop, after this.
    }
}

#[cfg(windows)]
mod win {
    use std::cell::UnsafeCell;
    use std::ptr::{null, null_mut};
    use std::time::Duration;

    use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
        DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, HDEVINFO, SP_DEVICE_INTERFACE_DATA,
        SP_DEVICE_INTERFACE_DETAIL_DATA_W, SetupDiDestroyDeviceInfoList,
        SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW, SetupDiGetDeviceInterfaceDetailW,
    };
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_DEVICE_NOT_CONNECTED, ERROR_FILE_NOT_FOUND,
        ERROR_GEN_FAILURE, ERROR_IO_PENDING, ERROR_NO_SUCH_DEVICE, ERROR_OPERATION_ABORTED,
        ERROR_PATH_NOT_FOUND, ERROR_SHARING_VIOLATION, FALSE, GENERIC_READ, GENERIC_WRITE,
        GetLastError, HANDLE, INVALID_HANDLE_VALUE, TRUE, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_OVERLAPPED, OPEN_EXISTING, ReadFile, WriteFile,
    };
    use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
    use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
    use windows_sys::core::GUID;

    use super::LinkError;

    /// `GUID_DEVINTERFACE_USBPRINT`.
    const USBPRINT: GUID = GUID::from_u128(0x28d78fad_5a12_11d1_ae5b_0000f803a8c2);
    /// Bytes asked for by one `ReadFile`. Generous on purpose: a buffer
    /// smaller than one high speed packet (512 bytes) can overflow in the
    /// driver, and whatever a read brings beyond one reply is kept for the
    /// next by the split at ETX.
    pub const READ_BYTES: usize = 4096;
    /// How long one write may take to be accepted. A block is at most a
    /// kilobyte and the machine is asked to be ready before each, so a write
    /// that has not gone in this long is not going to.
    const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
    /// More interfaces than this on one machine is a broken enumeration.
    const MAX_INTERFACES: u32 = 1024;

    /// A kernel handle owned by this value and closed when it goes.
    pub struct Handle(HANDLE);

    // SAFETY: a `Handle` is owned by exactly one value, which moves between
    // threads whole and is only ever used by the thread that holds it, and the
    // kernel object it names may be used from any thread. It is closed once, on
    // drop.
    unsafe impl Send for Handle {}

    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: `self.0` is a valid handle this value owns (it is never
            // constructed from an invalid one) and nothing uses it after this.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    /// Cancel every operation on the handle, before it is closed.
    pub fn cancel_all(handle: &Handle) {
        // SAFETY: a null OVERLAPPED asks for every outstanding operation on a
        // valid handle to be cancelled. Every operation this module starts has
        // already completed before its function returned, so this is a guard
        // against nothing being left rather than a cancellation anything waits on.
        unsafe {
            CancelIoEx(handle.0, null());
        }
    }

    fn last_error() -> u32 {
        // SAFETY: no preconditions; reads this thread's last error code.
        unsafe { GetLastError() }
    }

    /// `doing` is a lower-case clause, "writing" or "opening it", because the
    /// detail lands inside "The connection to the cutter failed: …".
    pub fn io(doing: &str, code: u32) -> LinkError {
        LinkError::Io(format!("Windows error {code} while {doing}"))
    }

    /// The device paths of every present USB printing interface.
    pub fn interface_paths() -> Vec<String> {
        // SAFETY: a valid GUID pointer, no enumerator, no window; the flags ask
        // for present device interfaces of that class.
        let set: HDEVINFO = unsafe {
            SetupDiGetClassDevsW(
                &USBPRINT,
                null(),
                null_mut(),
                DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
            )
        };
        if set == INVALID_HANDLE_VALUE as HDEVINFO {
            return Vec::new();
        }
        let set = DevInfo(set);
        let mut paths = Vec::new();
        for index in 0..MAX_INTERFACES {
            let mut data = SP_DEVICE_INTERFACE_DATA {
                cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
                ..Default::default()
            };
            // SAFETY: a live device information set, the class GUID it was
            // built for, and `data` with its size set as the call requires.
            let ok =
                unsafe { SetupDiEnumDeviceInterfaces(set.0, null(), &USBPRINT, index, &mut data) };
            if ok == FALSE {
                // ERROR_NO_MORE_ITEMS at the end of the list; any other error
                // also leaves nothing more to read.
                break;
            }
            if let Some(path) = detail_path(&set, &data) {
                paths.push(path);
            }
        }
        paths
    }

    /// The device path of one interface.
    fn detail_path(set: &DevInfo, data: &SP_DEVICE_INTERFACE_DATA) -> Option<String> {
        let header = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
        let mut needed = 0u32;
        // SAFETY: asking for the size alone: a null buffer of size zero, with
        // `needed` to receive the size. It is expected to fail with
        // ERROR_INSUFFICIENT_BUFFER, which is why the answer is not checked.
        unsafe {
            SetupDiGetDeviceInterfaceDetailW(set.0, data, null_mut(), 0, &mut needed, null_mut());
        }
        let needed_bytes = needed as usize;
        if needed_bytes < header {
            return None;
        }
        // Eight-byte cells, so the buffer is aligned for the struct on every
        // target and holds at least `needed` bytes.
        let mut buf = vec![0u64; needed_bytes.div_ceil(8)];
        let detail = buf.as_mut_ptr().cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
        // SAFETY: `detail` points to at least `header` writable bytes, aligned
        // for the struct. `cbSize` is the size of the fixed header (8 on 64-bit,
        // 6 on 32-bit, where the struct is packed), which is what the call
        // checks, and the unaligned write is right for the packed layout too.
        unsafe {
            std::ptr::addr_of_mut!((*detail).cbSize).write_unaligned(header as u32);
        }
        // SAFETY: the buffer is `needed` bytes long, as the first call asked.
        let ok = unsafe {
            SetupDiGetDeviceInterfaceDetailW(set.0, data, detail, needed, null_mut(), null_mut())
        };
        if ok == FALSE {
            return None;
        }
        let offset = std::mem::offset_of!(SP_DEVICE_INTERFACE_DETAIL_DATA_W, DevicePath);
        let chars = (needed_bytes - offset) / 2;
        // SAFETY: `DevicePath` starts `offset` bytes into a buffer of
        // `needed_bytes`, so `chars` u16s from there are inside it. A u16 read
        // is aligned because the buffer is eight-byte aligned and the offset
        // even (4 on every target).
        let wide = unsafe {
            std::slice::from_raw_parts(buf.as_ptr().cast::<u8>().add(offset).cast::<u16>(), chars)
        };
        let len = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
        Some(String::from_utf16_lossy(&wide[..len]))
    }

    /// A device information set, destroyed when it goes.
    struct DevInfo(HDEVINFO);

    impl Drop for DevInfo {
        fn drop(&mut self) {
            // SAFETY: a set returned by SetupDiGetClassDevsW, destroyed once.
            unsafe {
                SetupDiDestroyDeviceInfoList(self.0);
            }
        }
    }

    /// Open an interface for overlapped reads and writes, exclusively.
    pub fn open(path: &str) -> Result<Handle, LinkError> {
        let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: a NUL-terminated wide path, no sharing, no security
        // attributes, no template.
        let h = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE || h.is_null() {
            return Err(open_error(last_error()));
        }
        Ok(Handle(h))
    }

    /// What a failed open means. A machine unplugged or switched off between
    /// the enumeration and the open answers any of several codes, and each of
    /// them is "not found" to the artist.
    pub fn open_error(code: u32) -> LinkError {
        match code {
            ERROR_SHARING_VIOLATION | ERROR_ACCESS_DENIED => LinkError::Busy,
            ERROR_FILE_NOT_FOUND
            | ERROR_PATH_NOT_FOUND
            | ERROR_DEVICE_NOT_CONNECTED
            | ERROR_NO_SUCH_DEVICE
            | ERROR_GEN_FAILURE => LinkError::NotFound,
            code => io("opening it", code),
        }
    }

    /// A manual-reset event, unsignalled.
    fn event() -> Result<Handle, LinkError> {
        // SAFETY: no security attributes, manual reset, initially unsignalled,
        // no name.
        let h = unsafe { CreateEventW(null(), TRUE, FALSE, null()) };
        if h.is_null() {
            return Err(io("creating an event", last_error()));
        }
        Ok(Handle(h))
    }

    fn millis(d: Duration) -> u32 {
        d.as_millis().clamp(1, u128::from(u32::MAX - 1)) as u32
    }

    /// Wait for an overlapped operation for up to `wait`, and make sure it has
    /// finished before returning, cancelling it if it has not. Answers the
    /// bytes it moved and whether it finished on its own.
    ///
    /// The OVERLAPPED is reached through a raw pointer only: the kernel writes
    /// to it while the operation runs, so no Rust reference to it may be alive
    /// over that time.
    ///
    /// # Safety
    ///
    /// `ov` must point to the OVERLAPPED of an operation started on `handle`,
    /// with `event` its event, and both it and the buffer the operation uses
    /// must stay where they are until this returns, which is the whole point:
    /// once this returns the operation has completed and neither is in use.
    unsafe fn settle(
        handle: &Handle,
        ov: *const OVERLAPPED,
        event: &Handle,
        wait: Duration,
    ) -> Result<(u32, bool), u32> {
        // SAFETY: the event is a valid handle owned by the caller.
        let woke = unsafe { WaitForSingleObject(event.0, millis(wait)) };
        let mut moved = 0u32;
        if woke == WAIT_OBJECT_0 {
            // SAFETY: per this function's contract; the operation is complete,
            // so waiting returns at once.
            let ok = unsafe { GetOverlappedResult(handle.0, ov, &mut moved, TRUE) };
            if ok == FALSE {
                return Err(last_error());
            }
            return Ok((moved, true));
        }
        // Timed out, or the wait itself failed: in either case the operation
        // may still be running, and the buffer and the OVERLAPPED must not be
        // released until it is not.
        // SAFETY: cancels only this operation, which belongs to `handle`.
        unsafe {
            CancelIoEx(handle.0, ov);
        }
        // SAFETY: per this function's contract; waiting here is what makes the
        // operation finished before anything it uses goes away.
        let ok = unsafe { GetOverlappedResult(handle.0, ov, &mut moved, TRUE) };
        if ok == FALSE {
            let code = last_error();
            if code != ERROR_OPERATION_ABORTED {
                return Err(code);
            }
        }
        // Bytes that arrived before the cancel took are kept.
        Ok((moved, false))
    }

    /// A fresh OVERLAPPED for one operation, signalling `event`.
    fn overlapped(event: &Handle) -> UnsafeCell<OVERLAPPED> {
        UnsafeCell::new(OVERLAPPED {
            hEvent: event.0,
            ..Default::default()
        })
    }

    /// One overlapped read of up to `buf.len()` bytes, waited for up to `wait`.
    /// Answers how many arrived, which may be none.
    pub fn read(handle: &Handle, buf: &mut [u8], wait: Duration) -> Result<usize, LinkError> {
        let event = event()?;
        let ov = overlapped(&event);
        let len = buf.len().min(u32::MAX as usize) as u32;
        // SAFETY: a valid handle opened for overlapped reads, `len` writable
        // bytes at `buf`, no byte count pointer (an overlapped read reports it
        // through GetOverlappedResult), and an OVERLAPPED with its own event.
        // `buf` is borrowed for this whole function and `ov` and `event` are
        // locals, so all three stay put until `settle` has seen the operation
        // finish.
        let ok = unsafe { ReadFile(handle.0, buf.as_mut_ptr(), len, null_mut(), ov.get()) };
        if ok == FALSE {
            let code = last_error();
            if code != ERROR_IO_PENDING {
                return Err(io("reading", code));
            }
        }
        // SAFETY: `ov` belongs to the read just started on `handle`, `event` is
        // its event, and nothing here touches or releases `buf`, `ov` or
        // `event` until this returns.
        let (got, _) = unsafe { settle(handle, ov.get(), &event, wait) }
            .map_err(|code| io("reading", code))?;
        Ok((got as usize).min(buf.len()))
    }

    /// One overlapped write of all of `bytes`. Anything short of all of them is
    /// a failure, and the caller does not retry one.
    pub fn write(handle: &Handle, bytes: &[u8]) -> Result<(), LinkError> {
        if bytes.is_empty() {
            return Ok(());
        }
        let len = u32::try_from(bytes.len())
            .map_err(|_| LinkError::Io(format!("a write of {} bytes is too long", bytes.len())))?;
        let event = event()?;
        let ov = overlapped(&event);
        // SAFETY: a valid handle opened for overlapped writes, `len` readable
        // bytes at `bytes`, no byte count pointer, and an OVERLAPPED with its
        // own event; `bytes`, `ov` and `event` outlive the operation because
        // `settle` sees it finish before this returns.
        let ok = unsafe { WriteFile(handle.0, bytes.as_ptr(), len, null_mut(), ov.get()) };
        if ok == FALSE {
            let code = last_error();
            if code != ERROR_IO_PENDING {
                return Err(io("writing", code));
            }
        }
        // SAFETY: as for the read.
        let (wrote, finished) = unsafe { settle(handle, ov.get(), &event, WRITE_TIMEOUT) }
            .map_err(|code| io("writing", code))?;
        // A write cut off by the timeout may have delivered some of the block,
        // and that is exactly the case that must end the job rather than be
        // tried again.
        if !finished {
            return Err(LinkError::Timeout);
        }
        if wrote != len {
            return Err(LinkError::Io(format!(
                "only {wrote} of {len} bytes reached the cutter"
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    use glam::Vec2;
    use umber_core::cutjob::{self, JobSettings, Page};
    use umber_core::document::Document;
    use umber_core::vector::{Node, Path, PathRole};

    /// A machine that answers from a script. It replies to what was last
    /// written, the way the real one does: a firmware string to `FG`, the next
    /// queued status to a status query, the marks reply to a marks search.
    struct FakeLink {
        writes: Vec<Vec<u8>>,
        firmware: Vec<u8>,
        statuses: VecDeque<Vec<u8>>,
        /// What every status query answers once the queue is empty.
        idle: Vec<u8>,
        /// Once the queue is empty, status queries get no answer at all.
        silent: bool,
        marks: Vec<u8>,
        /// The first write equal to this fails, after being recorded.
        fail_on: Option<Vec<u8>>,
        /// The write at this index (counting from zero) fails, after being
        /// recorded.
        fail_at: Option<usize>,
        /// Unsolicited replies the machine queues after the first write equal
        /// to the key, behind that write's own reply.
        after: Vec<(Vec<u8>, Vec<Vec<u8>>)>,
        /// Replies that have arrived and not been read, oldest first. Nothing
        /// clears this but a read or a discard, which is what makes a leftover
        /// reply visible to the next read, as it would be on the wire.
        replies: VecDeque<Vec<u8>>,
        /// How many times `discard` was asked.
        discards: usize,
        /// Every write and every reply read, in the order they happened, so
        /// a test can say what the machine had answered before a write.
        trace: Vec<Trace>,
    }

    #[derive(Clone, Debug, PartialEq)]
    enum Trace {
        Wrote(Vec<u8>),
        Read(Vec<u8>),
    }

    impl FakeLink {
        fn new() -> Self {
            FakeLink {
                writes: Vec::new(),
                firmware: b"CAMEO V1.10    \x03".to_vec(),
                statuses: VecDeque::new(),
                idle: b"0\x03".to_vec(),
                silent: false,
                marks: MARKS_FOUND.to_vec(),
                fail_on: None,
                fail_at: None,
                after: Vec::new(),
                replies: VecDeque::new(),
                discards: 0,
                trace: Vec::new(),
            }
        }

        fn statuses(mut self, s: &[&[u8]]) -> Self {
            self.statuses = s.iter().map(|b| b.to_vec()).collect();
            self
        }

        fn after(mut self, write: &[u8], extra: &[&[u8]]) -> Self {
            self.after
                .push((write.to_vec(), extra.iter().map(|b| b.to_vec()).collect()));
            self
        }
    }

    impl Link for FakeLink {
        fn write(&mut self, bytes: &[u8]) -> Result<(), LinkError> {
            let index = self.writes.len();
            self.writes.push(bytes.to_vec());
            self.trace.push(Trace::Wrote(bytes.to_vec()));
            if self.fail_on.as_deref() == Some(bytes) {
                self.fail_on = None;
                return Err(LinkError::Io("scripted".into()));
            }
            if self.fail_at == Some(index) {
                return Err(LinkError::Io("scripted".into()));
            }
            let own = if bytes == FIRMWARE {
                Some(self.firmware.clone())
            } else if bytes == STATUS {
                match self.statuses.pop_front() {
                    Some(s) => Some(s),
                    None if self.silent => None,
                    None => Some(self.idle.clone()),
                }
            } else if bytes.starts_with(b"TB99") {
                Some(self.marks.clone())
            } else {
                None
            };
            self.replies.extend(own);
            if let Some(at) = self.after.iter().position(|(w, _)| w == bytes) {
                let (_, extra) = self.after.remove(at);
                self.replies.extend(extra);
            }
            Ok(())
        }

        fn read_reply(&mut self, _timeout: Duration) -> Result<Vec<u8>, LinkError> {
            let reply = self.replies.pop_front().ok_or(LinkError::Timeout)?;
            self.trace.push(Trace::Read(reply.clone()));
            Ok(reply)
        }

        fn discard(&mut self, _quiet: Duration) -> Result<Vec<u8>, LinkError> {
            self.discards += 1;
            Ok(self.replies.drain(..).flatten().collect())
        }
    }

    fn fast() -> Timing {
        Timing {
            poll: Duration::from_millis(1),
            ready: Duration::from_millis(30),
            marks: Duration::from_millis(5),
            reply: Duration::from_millis(5),
            settle: Duration::from_millis(1),
            drain: Duration::from_millis(1),
        }
    }

    /// A zigzag long enough to need several path blocks, on A4 at one pixel
    /// to the millimetre.
    fn job(use_marks: bool) -> Job {
        let mut doc = Document::new(210, 297).with_marks(true);
        doc.dpi = 25.4;
        let page = Page::of(&doc);
        let zigzag = Path {
            role: PathRole::Cut,
            closed: false,
            nodes: (0..400)
                .map(|i| {
                    let y = if i % 2 == 0 { 20.0 } else { 30.0 };
                    Node::corner(Vec2::new(20.0 + i as f32 * 0.25, y))
                })
                .collect(),
            ink: None,
        };
        let settings = JobSettings {
            use_marks,
            ..JobSettings::new(PathRole::Cut)
        };
        let job = cutjob::plan(&[&zigzag], &settings, &page).unwrap();
        assert!(job.path_blocks().len() >= 3, "{}", job.path_blocks().len());
        job
    }

    fn run(link: &mut FakeLink, job: &Job) -> (Result<String, Failure>, Vec<Stage>) {
        let stop = AtomicBool::new(false);
        let mut stages = Vec::new();
        let out = send(link, job, &fast(), &stop, &mut |s| stages.push(s));
        (out, stages)
    }

    fn count(writes: &[Vec<u8>], block: &[u8]) -> usize {
        writes.iter().filter(|w| w.as_slice() == block).count()
    }

    #[test]
    fn a_job_with_marks_is_written_in_order_and_once() {
        let job = job(true);
        let mut link = FakeLink::new();
        let (out, _) = run(&mut link, &job);
        assert_eq!(out.unwrap(), "CAMEO V1.10");

        let mut expected = vec![
            INIT.to_vec(),
            FIRMWARE.to_vec(),
            STATUS.to_vec(),
            job.setup_block(),
            STATUS.to_vec(),
            job.marks_block().unwrap(),
            STATUS.to_vec(),
        ];
        for b in job.path_blocks() {
            expected.push(b);
            expected.push(STATUS.to_vec());
        }
        expected.push(job.end_block());
        expected.push(STATUS.to_vec());
        assert_eq!(link.writes, expected);

        assert_eq!(count(&link.writes, &job.setup_block()), 1);
        assert_eq!(count(&link.writes, &job.marks_block().unwrap()), 1);
        for b in job.path_blocks() {
            assert_eq!(count(&link.writes, &b), 1);
        }
        assert_eq!(count(&link.writes, &job.end_block()), 1);
    }

    /// A successful search answers three times, the reference implementation
    /// says. Only the first is the verdict; the other two must not be read as
    /// the status of anything.
    #[test]
    fn extra_replies_after_the_marks_search_do_not_derail_the_job() {
        let job = job(true);
        let marks = job.marks_block().unwrap();
        let mut link = FakeLink::new().after(&marks, &[b"    0\x03", b"    1\x03"]);
        let (out, _) = run(&mut link, &job);
        assert_eq!(out.unwrap(), "CAMEO V1.10");
        assert_eq!(count(&link.writes, &job.setup_block()), 1);
        assert_eq!(count(&link.writes, &marks), 1);
        let blocks = job.path_blocks();
        for b in &blocks {
            assert_eq!(count(&link.writes, b), 1);
        }
        assert_eq!(count(&link.writes, &job.end_block()), 1);
        // In order: every path block after the marks, the end block last.
        let at = |b: &[u8]| link.writes.iter().position(|w| w == b).unwrap();
        let mut last = at(&marks);
        for b in &blocks {
            assert!(at(b) > last);
            last = at(b);
        }
        assert!(at(&job.end_block()) > last);
    }

    #[test]
    fn a_leftover_reply_is_never_taken_for_a_status() {
        let job = job(true);
        let setup = job.setup_block();
        // Ready before setup, then Moving once after it, with a stale ready
        // reply already queued behind the setup block.
        let mut link = FakeLink::new()
            .statuses(&[b"0\x03", b"1\x03"])
            .after(&setup, &[b"0\x03"]);
        let (out, _) = run(&mut link, &job);
        assert!(out.is_ok(), "{out:?}");
        let at = link.writes.iter().position(|w| *w == setup).unwrap();
        // Moving was read, so the machine was asked again before the marks.
        assert_eq!(link.writes[at + 1], STATUS);
        assert_eq!(link.writes[at + 2], STATUS);
        assert_eq!(link.writes[at + 3], job.marks_block().unwrap());
    }

    #[test]
    fn the_first_cut_waits_for_the_machine_to_be_ready_after_the_search() {
        let job = job(true);
        let marks = job.marks_block().unwrap();
        // Ready before and after setup, then Moving twice after the search.
        let mut link = FakeLink::new().statuses(&[b"0\x03", b"0\x03", b"1\x03", b"1\x03"]);
        let (out, _) = run(&mut link, &job);
        assert!(out.is_ok(), "{out:?}");
        let at = link.writes.iter().position(|w| *w == marks).unwrap();
        assert_eq!(&link.writes[at + 1..at + 4], vec![STATUS.to_vec(); 3]);
        assert_eq!(link.writes[at + 4], job.path_blocks()[0]);
    }

    #[test]
    fn a_long_block_is_waited_for_while_the_machine_says_moving() {
        // One block of a kilobyte can take minutes; a machine still answering
        // Moving is alive and is waited for.
        assert!(Timing::REAL.ready >= Duration::from_secs(45 * 60));
        let job = job(true);
        let mut statuses: Vec<&[u8]> = vec![b"0\x03"; 3];
        statuses.extend(std::iter::repeat_n(&b"1\x03"[..], 200));
        let mut link = FakeLink::new().statuses(&statuses);
        let timing = Timing {
            ready: Duration::from_secs(60),
            ..fast()
        };
        let stop = AtomicBool::new(false);
        let out = send(&mut link, &job, &timing, &stop, &mut |_| {});
        assert!(out.is_ok(), "{out:?}");
        for b in job.path_blocks() {
            assert_eq!(count(&link.writes, &b), 1);
        }
    }

    /// After a stop the end block is written exactly once, only once the
    /// machine has answered Ready to the query just before it, and nothing
    /// follows it but the wait for it to finish.
    fn sent_home_once_after_ready(link: &FakeLink, job: &Job) {
        let end = job.end_block();
        assert_eq!(count(&link.writes, &end), 1, "{:?}", link.writes);
        let at = link
            .trace
            .iter()
            .position(|t| *t == Trace::Wrote(end.clone()))
            .unwrap();
        assert!(at >= 2, "{:?}", link.trace);
        assert_eq!(link.trace[at - 2], Trace::Wrote(STATUS.to_vec()));
        assert_eq!(link.trace[at - 1], Trace::Read(b"0\x03".to_vec()));
        assert_eq!(
            &link.trace[at + 1..],
            &[
                Trace::Wrote(STATUS.to_vec()),
                Trace::Read(b"0\x03".to_vec())
            ]
        );
    }

    /// A stop seen while the machine is still cutting a block lets it finish
    /// that block, however long it says Moving, and then sends it home.
    #[test]
    fn stop_during_a_wait_lets_the_block_finish_and_then_sends_the_machine_home() {
        let job = job(true);
        let blocks = job.path_blocks();
        // Ready before setup, after setup and after the search; then Moving
        // five times after the first block, then Ready from there on.
        let mut statuses: Vec<&[u8]> = vec![b"0\x03"; 3];
        statuses.extend(std::iter::repeat_n(&b"1\x03"[..], 5));
        let mut link = FakeLink::new().statuses(&statuses);
        let stop = AtomicBool::new(false);
        let mut stages = Vec::new();
        let out = send(&mut link, &job, &fast(), &stop, &mut |s| {
            stages.push(s);
            if matches!(s, Stage::Cutting { block: 0, .. }) {
                stop.store(true, Ordering::SeqCst);
            }
        });
        let failure = out.unwrap_err();
        assert!(matches!(failure.error, SendError::Stopped), "{failure:?}");
        assert_eq!(failure.blocks_sent, 1);
        assert_eq!(count(&link.writes, &blocks[0]), 1);
        for b in &blocks[1..] {
            assert_eq!(count(&link.writes, b), 0);
        }
        // Every Moving was waited through: none of them was taken for the
        // end of the job.
        assert!(link.statuses.is_empty(), "{:?}", link.statuses);
        sent_home_once_after_ready(&link, &job);
        // Said once, after the block that was cut.
        assert_eq!(
            stages.iter().filter(|s| **s == Stage::Stopping).count(),
            1,
            "{stages:?}"
        );
        let cut = stages
            .iter()
            .position(|s| matches!(s, Stage::Cutting { block: 0, .. }))
            .unwrap();
        assert_eq!(stages[cut + 1], Stage::Stopping, "{stages:?}");
        assert!(!stages.contains(&Stage::Finishing), "{stages:?}");
    }

    /// Whatever goes wrong while a stopped job is waited for is reported as
    /// it is, with the blocks already sent.
    #[test]
    fn a_failure_while_stopping_is_that_failure() {
        let job = job(true);
        let mut link = FakeLink::new().statuses(&[&b"0\x03"[..], b"0\x03", b"0\x03", b"3\x03"]);
        let stop = AtomicBool::new(false);
        let failure = send(&mut link, &job, &fast(), &stop, &mut |s| {
            if matches!(s, Stage::Cutting { block: 0, .. }) {
                stop.store(true, Ordering::SeqCst);
            }
        })
        .unwrap_err();
        match &failure.error {
            SendError::Unknown(raw) => assert_eq!(raw, b"3\x03"),
            other => panic!("{other:?}"),
        }
        assert_eq!(failure.blocks_sent, 1);
        assert_eq!(count(&link.writes, &job.end_block()), 0);

        // Moving for ever after a stop is NeverReady, not Stopped.
        let mut link = FakeLink::new().statuses(&[&b"0\x03"[..]; 3]);
        link.idle = b"1\x03".to_vec();
        let stop = AtomicBool::new(false);
        let failure = send(&mut link, &job, &fast(), &stop, &mut |s| {
            if matches!(s, Stage::Cutting { block: 0, .. }) {
                stop.store(true, Ordering::SeqCst);
            }
        })
        .unwrap_err();
        assert!(
            matches!(failure.error, SendError::NeverReady),
            "{failure:?}"
        );
        assert_eq!(failure.blocks_sent, 1);
        assert_eq!(count(&link.writes, &job.end_block()), 0);

        // The end block's own write failing is that failure too.
        let mut link = FakeLink::new();
        link.fail_on = Some(job.end_block());
        let stop = AtomicBool::new(true);
        let failure = send(&mut link, &job, &fast(), &stop, &mut |_| {}).unwrap_err();
        assert!(
            matches!(failure.error, SendError::Link(LinkError::Io(_))),
            "{failure:?}"
        );
        assert_eq!(failure.blocks_sent, 0);
        assert_eq!(link.writes.last(), Some(&job.end_block()));
    }

    const PART_CUT: &str = " Part of the sheet may already be cut. Check it before sending again.";

    #[test]
    fn a_failure_after_the_first_cut_says_the_sheet_may_be_part_cut() {
        let job = job(true);
        let blocks = job.path_blocks();
        let n = blocks.len();

        // The second block's write fails: it may have gone in part, so it
        // counts as sent.
        let mut link = FakeLink::new();
        link.fail_on = Some(blocks[1].clone());
        let (out, _) = run(&mut link, &job);
        let failure = out.unwrap_err();
        assert!(matches!(failure.error, SendError::Link(LinkError::Io(_))));
        assert_eq!((failure.blocks_sent, failure.blocks), (2, n));
        assert_eq!(
            failure.sentence(),
            format!("{}{PART_CUT}", failure.error.sentence())
        );

        // An answer Umber does not know after the first block.
        let mut link = FakeLink::new().statuses(&[&b"0\x03"[..], b"0\x03", b"0\x03", b"3\x03"]);
        let (out, _) = run(&mut link, &job);
        let failure = out.unwrap_err();
        assert!(matches!(failure.error, SendError::Unknown(_)));
        assert_eq!((failure.blocks_sent, failure.blocks), (1, n));
        assert!(
            failure.sentence().ends_with(PART_CUT),
            "{}",
            failure.sentence()
        );

        // Stopped after the last block: the end block is still sent once,
        // and that is a stop, not a whole job.
        let mut link = FakeLink::new();
        let stop = AtomicBool::new(false);
        let failure = send(&mut link, &job, &fast(), &stop, &mut |s| {
            if s == (Stage::Cutting {
                block: n - 1,
                of: n,
            }) {
                stop.store(true, Ordering::SeqCst);
            }
        })
        .unwrap_err();
        assert!(matches!(failure.error, SendError::Stopped), "{failure:?}");
        assert_eq!(failure.blocks_sent, n);
        sent_home_once_after_ready(&link, &job);
        assert!(failure.sentence().ends_with(PART_CUT));
    }

    #[test]
    fn a_failure_before_any_cut_does_not_say_so() {
        let job = job(true);
        let n = job.path_blocks().len();
        let mut link = FakeLink::new();
        link.marks = b"    1\x03".to_vec();
        let (out, _) = run(&mut link, &job);
        let failure = out.unwrap_err();
        assert!(matches!(failure.error, SendError::MarksNotFound(_)));
        assert_eq!((failure.blocks_sent, failure.blocks), (0, n));
        assert_eq!(failure.sentence(), failure.error.sentence());
        assert!(!failure.sentence().contains("Part of the sheet"));

        let mut link = FakeLink::new();
        link.fail_on = Some(job.setup_block());
        let (out, _) = run(&mut link, &job);
        let failure = out.unwrap_err();
        assert_eq!(failure.blocks_sent, 0);
        assert_eq!(failure.sentence(), failure.error.sentence());
    }

    #[test]
    fn a_job_sent_whole_but_never_confirmed_says_exactly_that() {
        const SENTENCE: &str = "The whole job was sent, but the cutter did not confirm it had \
                                finished. Check the sheet before sending again.";
        let job = job(true);
        let n = job.path_blocks().len();
        // Before setup, after setup, after the search, and after each block.
        let ready = vec![&b"0\x03"[..]; n + 3];

        let check = |link: &FakeLink, failure: &Failure| {
            assert_eq!(failure.blocks_sent, n);
            assert_eq!(failure.blocks, n);
            assert_eq!(failure.sentence(), SENTENCE);
            assert_eq!(failure.error.sentence(), SENTENCE);
            assert_eq!(count(&link.writes, &job.end_block()), 1);
        };

        // An unknown answer to the last poll.
        let mut link = FakeLink::new().statuses(&ready);
        link.idle = b"3\x03".to_vec();
        let failure = run(&mut link, &job).0.unwrap_err();
        match &failure.error {
            SendError::Unconfirmed(raw) => assert_eq!(raw, b"3\x03"),
            other => panic!("{other:?}"),
        }
        check(&link, &failure);

        // No answer at all.
        let mut link = FakeLink::new().statuses(&ready);
        link.silent = true;
        let failure = run(&mut link, &job).0.unwrap_err();
        assert!(matches!(failure.error, SendError::Unconfirmed(_)));
        check(&link, &failure);

        // Moving past the deadline.
        let mut link = FakeLink::new().statuses(&ready);
        link.idle = b"1\x03".to_vec();
        let failure = run(&mut link, &job).0.unwrap_err();
        assert!(matches!(failure.error, SendError::Unconfirmed(_)));
        check(&link, &failure);
    }

    /// The fake answers by the production constants, so a wrong constant
    /// would pass every other test. These are the bytes the machine is
    /// documented to want, written out.
    #[test]
    fn the_harmless_commands_are_these_exact_bytes() {
        let mut link = FakeLink::new();
        let (_, status) = probe(&mut link, &fast()).unwrap();
        assert_eq!(status, Ok(Status::Ready));
        assert_eq!(
            link.writes,
            vec![
                b"\x1b\x04".to_vec(),
                b"FG\x03".to_vec(),
                b"\x1b\x05".to_vec()
            ]
        );

        let job = job(true);
        let mut link = FakeLink::new();
        run(&mut link, &job).0.unwrap();
        assert_eq!(
            &link.writes[..3],
            &[
                b"\x1b\x04".to_vec(),
                b"FG\x03".to_vec(),
                b"\x1b\x05".to_vec()
            ]
        );
        assert_eq!(MARKS_FOUND, b"    0\x03");
    }

    #[test]
    fn a_failed_write_anywhere_ends_the_job_at_once() {
        let job = job(true);
        let n = job.path_blocks().len();
        let check = |link: &FakeLink, failed: &[u8], at: usize| {
            assert_eq!(link.writes.len(), at + 1, "{:?}", link.writes);
            assert_eq!(link.writes[at], failed);
        };
        for (block, sent) in [
            (job.setup_block(), 0),
            (job.marks_block().unwrap(), 0),
            (job.end_block(), n),
        ] {
            let mut link = FakeLink::new();
            link.fail_on = Some(block.clone());
            let failure = run(&mut link, &job).0.unwrap_err();
            assert!(
                matches!(failure.error, SendError::Link(LinkError::Io(_))),
                "{failure:?}"
            );
            assert_eq!(failure.blocks_sent, sent);
            assert_eq!(count(&link.writes, &block), 1);
            let at = link.writes.iter().position(|w| *w == block).unwrap();
            check(&link, &block, at);
        }

        // The status query after the first block: init, firmware, status,
        // setup, status, marks, status, block 0, and then this one.
        let mut link = FakeLink::new();
        link.fail_at = Some(8);
        let failure = run(&mut link, &job).0.unwrap_err();
        assert!(matches!(failure.error, SendError::Link(LinkError::Io(_))));
        assert_eq!(failure.blocks_sent, 1);
        assert_eq!(link.writes[7], job.path_blocks()[0]);
        check(&link, STATUS, 8);
    }

    #[cfg(windows)]
    #[test]
    fn a_windows_error_reads_as_one_sentence() {
        let s = SendError::Link(win::io("writing", 31)).sentence();
        assert_eq!(
            s,
            "The connection to the cutter failed: Windows error 31 while writing."
        );
        assert_eq!(s.matches("failed").count(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn a_machine_that_is_gone_on_open_is_not_found() {
        for code in [2, 3, 31, 433, 1167] {
            assert!(
                matches!(win::open_error(code), LinkError::NotFound),
                "{code}"
            );
        }
        for code in [5, 32] {
            assert!(matches!(win::open_error(code), LinkError::Busy), "{code}");
        }
        assert!(matches!(win::open_error(87), LinkError::Io(_)));
        // A read buffer smaller than a high speed packet can overflow.
        const { assert!(win::READ_BYTES >= 4096) };
    }

    #[test]
    fn marks_not_found_ends_the_job_before_any_cut() {
        let job = job(true);
        let mut link = FakeLink::new();
        link.marks = b"    1\x03".to_vec();
        let (out, _) = run(&mut link, &job);
        let out = out.map_err(|f| f.error);
        match out {
            Err(SendError::MarksNotFound(raw)) => assert_eq!(raw, b"    1\x03"),
            other => panic!("{other:?}"),
        }
        let at = link
            .writes
            .iter()
            .position(|w| *w == job.marks_block().unwrap())
            .unwrap();
        assert!(
            link.writes[at + 1..].iter().all(|w| !w.contains(&b'D')),
            "{:?}",
            &link.writes[at + 1..]
        );
        assert_eq!(at, link.writes.len() - 1);
    }

    #[test]
    fn a_marks_reply_with_other_spacing_is_not_found() {
        assert!(marks_found(b"    0\x03"));
        for reply in [&b"0\x03"[..], b"   0\x03", b"     0\x03", b"    0"] {
            assert!(!marks_found(reply), "{reply:?}");
            let job = job(true);
            let mut link = FakeLink::new();
            link.marks = reply.to_vec();
            let (out, _) = run(&mut link, &job);
            let out = out.map_err(|f| f.error);
            match out {
                Err(SendError::MarksNotFound(raw)) => assert_eq!(raw, reply),
                other => panic!("{reply:?}: {other:?}"),
            }
            assert!(
                job.path_blocks()
                    .iter()
                    .all(|b| count(&link.writes, b) == 0)
            );
        }
    }

    #[test]
    fn no_media_is_said_before_anything_is_set_up() {
        let job = job(true);
        let mut link = FakeLink::new().statuses(&[b"2\x03"]);
        let (out, _) = run(&mut link, &job);
        let out = out.map_err(|f| f.error);
        assert!(matches!(out, Err(SendError::NoMedia)), "{out:?}");
        assert_eq!(
            link.writes,
            vec![INIT.to_vec(), FIRMWARE.to_vec(), STATUS.to_vec()]
        );
    }

    #[test]
    fn an_unknown_status_is_reported_raw_and_never_ready() {
        for raw in [&b"3\x03"[..], b"4\x03", b"\x03", b"", b" 0\x03", b"0"] {
            assert!(status_of(raw).is_err(), "{raw:?}");
            let job = job(true);
            let mut link = FakeLink::new().statuses(&[raw]);
            let (out, _) = run(&mut link, &job);
            let out = out.map_err(|f| f.error);
            match out {
                Err(SendError::Unknown(got)) => assert_eq!(got, raw),
                other => panic!("{raw:?}: {other:?}"),
            }
            assert_eq!(
                link.writes,
                vec![INIT.to_vec(), FIRMWARE.to_vec(), STATUS.to_vec()]
            );
        }
        assert_eq!(status_of(b"0\x03"), Ok(Status::Ready));
        assert_eq!(status_of(b"1\x03"), Ok(Status::Moving));
        assert_eq!(status_of(b"2\x03"), Ok(Status::NoMedia));
    }

    #[test]
    fn moving_keeps_polling_and_then_gives_up() {
        let job = job(true);

        // Moving for a while and then ready: the job goes on.
        let mut link = FakeLink::new().statuses(&[b"1\x03", b"1\x03", b"1\x03"]);
        let (out, _) = run(&mut link, &job);
        assert!(out.is_ok(), "{out:?}");
        assert_eq!(&link.writes[2..6], vec![STATUS.to_vec(); 4].as_slice());
        assert_eq!(link.writes[6], job.setup_block());

        // Moving for ever: polled more than once, then given up, nothing set up.
        let mut link = FakeLink::new();
        link.idle = b"1\x03".to_vec();
        let started = Instant::now();
        let (out, _) = run(&mut link, &job);
        let out = out.map_err(|f| f.error);
        assert!(matches!(out, Err(SendError::NeverReady)), "{out:?}");
        assert!(started.elapsed() >= fast().ready);
        assert!(count(&link.writes, STATUS) > 1);
        assert_eq!(count(&link.writes, &job.setup_block()), 0);
    }

    #[test]
    fn a_failed_write_is_never_retried() {
        let job = job(true);
        let second = job.path_blocks()[1].clone();
        let mut link = FakeLink::new();
        link.fail_on = Some(second.clone());
        let (out, _) = run(&mut link, &job);
        let out = out.map_err(|f| f.error);
        assert!(
            matches!(out, Err(SendError::Link(LinkError::Io(_)))),
            "{out:?}"
        );
        assert_eq!(count(&link.writes, &second), 1);
        assert_eq!(link.writes.last(), Some(&second));
    }

    #[test]
    fn a_job_without_marks_sends_no_search() {
        let job = job(false);
        assert_eq!(job.marks_block(), None);
        let mut link = FakeLink::new();
        let (out, stages) = run(&mut link, &job);
        assert!(out.is_ok(), "{out:?}");
        assert!(link.writes.iter().all(|w| !w.starts_with(b"TB99")));
        assert!(
            link.writes
                .iter()
                .all(|w| !w.windows(5).any(|x| x == b"TB123"))
        );
        assert!(!stages.contains(&Stage::FindingMarks));
        assert_eq!(link.writes[4], STATUS.to_vec());
        assert_eq!(link.writes[5], job.path_blocks()[0]);
    }

    #[test]
    fn progress_reports_each_stage_in_order() {
        let job = job(true);
        let n = job.path_blocks().len();
        let mut link = FakeLink::new();
        let (out, stages) = run(&mut link, &job);
        assert!(out.is_ok(), "{out:?}");
        let mut expected = vec![Stage::Connecting, Stage::FindingMarks];
        expected.extend((0..n).map(|block| Stage::Cutting { block, of: n }));
        expected.push(Stage::Finishing);
        assert_eq!(stages, expected);
    }

    #[test]
    fn stop_writes_no_further_path_block_and_sends_the_machine_home() {
        let job = job(true);
        let blocks = job.path_blocks();
        let mut link = FakeLink::new();
        let stop = AtomicBool::new(false);
        let out = send(&mut link, &job, &fast(), &stop, &mut |s| {
            if s == (Stage::Cutting {
                block: 0,
                of: blocks.len(),
            }) {
                stop.store(true, Ordering::SeqCst);
            }
        });
        let out = out.map_err(|f| f.error);
        assert!(matches!(out, Err(SendError::Stopped)), "{out:?}");
        assert_eq!(count(&link.writes, &blocks[0]), 1);
        for b in &blocks[1..] {
            assert_eq!(count(&link.writes, b), 0);
        }
        sent_home_once_after_ready(&link, &job);
        // After the block that was sent, nothing but status queries and the
        // end block.
        let at = link.writes.iter().position(|w| *w == blocks[0]).unwrap();
        assert!(
            link.writes[at + 1..]
                .iter()
                .all(|w| w == STATUS || *w == job.end_block()),
            "{:?}",
            &link.writes[at + 1..]
        );

        // Raised before the job begins: nothing is set up, searched or cut,
        // and the machine is still sent home once it is ready.
        let mut link = FakeLink::new();
        let stop = AtomicBool::new(true);
        let out = send(&mut link, &job, &fast(), &stop, &mut |_| {});
        let out = out.map_err(|f| f.error);
        assert!(matches!(out, Err(SendError::Stopped)), "{out:?}");
        assert_eq!(count(&link.writes, &job.setup_block()), 0);
        assert_eq!(count(&link.writes, &job.marks_block().unwrap()), 0);
        assert!(blocks.iter().all(|b| count(&link.writes, b) == 0));
        sent_home_once_after_ready(&link, &job);
    }

    #[test]
    fn every_send_error_has_its_own_sentence_and_no_dash() {
        // Exhaustive on purpose: a new variant must be added here to compile.
        fn every() -> Vec<SendError> {
            let link = |e: &LinkError| match e {
                LinkError::NotFound
                | LinkError::Busy
                | LinkError::Timeout
                | LinkError::Io(_)
                | LinkError::Unsupported => (),
            };
            let errors = vec![
                LinkError::NotFound,
                LinkError::Busy,
                LinkError::Timeout,
                LinkError::Io("the device was removed".into()),
                LinkError::Unsupported,
            ];
            errors.iter().for_each(link);
            let mut all: Vec<SendError> = errors.into_iter().map(SendError::Link).collect();
            for e in [
                SendError::NoMedia,
                SendError::MarksNotFound(b"    1\x03".to_vec()),
                SendError::MarksTimedOut,
                SendError::Unknown(b"3\x03".to_vec()),
                SendError::NeverReady,
                SendError::Stopped,
                SendError::Unconfirmed(b"3\x03".to_vec()),
            ] {
                match e {
                    SendError::Link(_)
                    | SendError::NoMedia
                    | SendError::MarksNotFound(_)
                    | SendError::MarksTimedOut
                    | SendError::Unknown(_)
                    | SendError::NeverReady
                    | SendError::Stopped
                    | SendError::Unconfirmed(_) => all.push(e),
                }
            }
            all
        }
        let sentences: Vec<String> = every().iter().map(SendError::sentence).collect();
        assert_eq!(sentences.len(), 12);
        // A timeout can be a write as well as a read, so it may not say the
        // cutter stopped answering.
        assert_eq!(sentences[2], "The cutter did not respond in time.");
        for (i, s) in sentences.iter().enumerate() {
            assert!(!s.is_empty());
            assert!(s.ends_with('.'), "{s}");
            assert!(!s.contains(['-', '\u{2013}', '\u{2014}']), "{s}");
            for t in &sentences[i + 1..] {
                assert_ne!(s, t);
            }
        }
        assert_eq!(
            sentences[6],
            "The cutter did not report finding the registration marks, so nothing was cut. \
             It answered:     1\\x03."
        );
        assert_eq!(
            sentences[7],
            "The cutter did not answer while looking for the registration marks. Nothing \
             was cut."
        );
        assert!(sentences[8].contains("3\\x03"), "{}", sentences[8]);
        assert_eq!(
            sentences[10],
            "Stopped. The cutter finished the part it had already been sent, and nothing \
             more was sent."
        );
        // One wording for the platform limit, here and in the dialog.
        assert_eq!(sentences[4], crate::cutterdlg::WINDOWS_ONLY);
    }

    /// A search that gets no reply is said as such, not as a generic timeout,
    /// and nothing is cut after it.
    #[test]
    fn a_marks_search_with_no_reply_says_so() {
        let job = job(true);
        let mut link = FakeLink::new();
        // A reply the search never gets: the queue is emptied by the discard
        // before the search's own read, so nothing comes.
        link.marks = Vec::new();
        link.after.clear();
        let marks = job.marks_block().unwrap();
        let mut silent = SilentSearch(link, marks.clone());
        let stop = AtomicBool::new(false);
        let failure = send(&mut silent, &job, &fast(), &stop, &mut |_| {}).unwrap_err();
        assert!(
            matches!(failure.error, SendError::MarksTimedOut),
            "{failure:?}"
        );
        assert_eq!(failure.blocks_sent, 0);
        assert!(
            job.path_blocks()
                .iter()
                .all(|b| count(&silent.0.writes, b) == 0)
        );
        assert_eq!(silent.0.writes.last(), Some(&marks));
    }

    /// The scripted machine, except that the marks search is never answered.
    struct SilentSearch(FakeLink, Vec<u8>);

    impl Link for SilentSearch {
        fn write(&mut self, bytes: &[u8]) -> Result<(), LinkError> {
            self.0.write(bytes)?;
            if bytes == self.1.as_slice() {
                self.0.replies.clear();
            }
            Ok(())
        }
        fn read_reply(&mut self, timeout: Duration) -> Result<Vec<u8>, LinkError> {
            self.0.read_reply(timeout)
        }
        fn discard(&mut self, quiet: Duration) -> Result<Vec<u8>, LinkError> {
            self.0.discard(quiet)
        }
    }

    #[test]
    fn probe_sends_only_the_three_harmless_commands() {
        let mut link = FakeLink::new().statuses(&[b"1\x03"]);
        let (firmware, status) = probe(&mut link, &fast()).unwrap();
        assert_eq!(firmware, "CAMEO V1.10");
        assert_eq!(status, Ok(Status::Moving));
        assert_eq!(
            link.writes,
            vec![INIT.to_vec(), FIRMWARE.to_vec(), STATUS.to_vec()]
        );

        let mut link = FakeLink::new().statuses(&[b"7\x03"]);
        let (_, status) = probe(&mut link, &fast()).unwrap();
        assert_eq!(status, Err(b"7\x03".to_vec()));
    }

    #[test]
    fn the_log_shows_each_transfer_once_with_its_direction() {
        let mut lines = Vec::new();
        let mut link = Logged::new(FakeLink::new(), |l| lines.push(l));
        link.write(b"FG\x03").unwrap();
        let reply = link.read_reply(Duration::from_millis(1)).unwrap();
        assert_eq!(reply, b"CAMEO V1.10    \x03");
        assert!(link.read_reply(Duration::from_millis(1)).is_err());
        drop(link);
        assert_eq!(
            lines,
            vec![
                "> 3 FG\\x03".to_string(),
                "< 16 CAMEO V1.10    \\x03".to_string(),
                "< failed: Timeout".to_string(),
            ]
        );
    }

    #[test]
    fn a_discard_is_logged_only_when_it_threw_something_away() {
        let mut lines = Vec::new();
        let fake = FakeLink::new().after(b"FG\x03", &[b"    0\x03"]);
        let mut link = Logged::new(fake, |l| lines.push(l));
        assert_eq!(link.discard(Duration::ZERO).unwrap(), b"");
        link.write(b"FG\x03").unwrap();
        let thrown = link.discard(Duration::ZERO).unwrap();
        assert_eq!(thrown, b"CAMEO V1.10    \x03    0\x03");
        drop(link);
        assert_eq!(
            lines,
            vec![
                "> 3 FG\\x03".to_string(),
                "x 22 CAMEO V1.10    \\x03    0\\x03".to_string(),
            ]
        );
    }

    #[test]
    fn a_long_write_is_logged_cut_short_with_its_length() {
        let mut lines = Vec::new();
        let bytes = vec![b'D'; 200];
        let mut link = Logged::new(FakeLink::new(), |l| lines.push(l));
        link.write(&bytes).unwrap();
        drop(link);
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0],
            format!("> 200 {} ... (200 bytes)", "D".repeat(96))
        );
    }

    #[test]
    fn control_bytes_are_escaped() {
        assert_eq!(escaped(b"\x1b\x04"), "\\x1b\\x04");
        assert_eq!(escaped(b"\\0,0\x03"), "\\\\0,0\\x03");
        assert_eq!(escaped(b"a b~\x7f\xff\x00"), "a b~\\x7f\\xff\\x00");

        let mut lines = Vec::new();
        let mut link = Logged::new(FakeLink::new(), |l| lines.push(l));
        link.write(b"\x1b\x05").unwrap();
        drop(link);
        assert_eq!(lines, vec!["> 2 \\x1b\\x05".to_string()]);

        // A failed write is still one line, and says it failed.
        let mut lines = Vec::new();
        let mut fake = FakeLink::new();
        fake.fail_on = Some(b"M0,0\x03".to_vec());
        let mut link = Logged::new(fake, |l| lines.push(l));
        assert!(link.write(b"M0,0\x03").is_err());
        drop(link);
        assert_eq!(
            lines,
            vec!["> 5 M0,0\\x03 failed: Io(\"scripted\")".to_string()]
        );
    }

    #[test]
    fn the_cutter_is_told_from_a_printer_by_its_ids() {
        const GUID: &str = "{28d78fad-5a12-11d1-ae5b-0000f803a8c2}";
        let portrait = format!("\\\\?\\usb#vid_0b4d&pid_113a#7&1a2b3c&0&1#{GUID}");
        let other = format!("\\\\?\\usb#vid_0b4d&pid_1137#5&9f&0&2#{GUID}");
        let printer = format!("\\\\?\\usb#vid_04b8&pid_0005#x#{GUID}");
        let shouting = format!("\\\\?\\USB#VID_0B4D&PID_113A#7&1A2B#{GUID}");
        // A longer id that merely starts with Silhouette's, and an id that
        // only appears in a serial number.
        let longer = format!("\\\\?\\usb#vid_0b4d1&pid_113a#x#{GUID}");
        let in_serial = format!("\\\\?\\usb#vid_04b8&pid_0005#vid_0b4d&pid_113a#{GUID}");

        assert!(is_cutter(&portrait, Some(PORTRAIT_3)));
        assert!(is_cutter(&portrait, None));
        assert!(is_cutter(&shouting, Some(PORTRAIT_3)));
        assert!(!is_cutter(&other, Some(PORTRAIT_3)));
        assert!(is_cutter(&other, None));
        assert!(!is_cutter(&printer, None));
        assert!(!is_cutter(&printer, Some(PORTRAIT_3)));
        assert!(!is_cutter(&longer, None));
        assert!(!is_cutter(&in_serial, None));
        assert!(!is_cutter("", None));
    }

    #[test]
    fn a_reply_is_split_at_etx_and_the_rest_kept() {
        let mut pending = b"    0\x030\x03CAM".to_vec();
        assert_eq!(take_reply(&mut pending), Some(b"    0\x03".to_vec()));
        assert_eq!(take_reply(&mut pending), Some(b"0\x03".to_vec()));
        assert_eq!(take_reply(&mut pending), None);
        assert_eq!(pending, b"CAM");
        pending.extend_from_slice(b"EO\x03");
        assert_eq!(take_reply(&mut pending), Some(b"CAMEO\x03".to_vec()));
        assert!(pending.is_empty());
    }

    /// The worker the dialog sends a job from owns the link, so it must be
    /// able to take it to another thread.
    #[test]
    fn the_usb_link_can_go_to_a_worker_thread() {
        fn send_able<T: Send>() {}
        send_able::<UsbPrint>();
        send_able::<Box<dyn Link + Send>>();
    }

    /// The worker the dialog drives, against the same scripted machine.
    mod worker {
        use super::*;
        use std::sync::atomic::AtomicUsize;
        use std::sync::{Arc, Mutex};

        /// A scripted machine the test can still read once the worker has
        /// taken it to its own thread.
        struct Shared(Arc<Mutex<FakeLink>>);

        impl Link for Shared {
            fn write(&mut self, bytes: &[u8]) -> Result<(), LinkError> {
                self.0.lock().unwrap().write(bytes)
            }
            fn read_reply(&mut self, timeout: Duration) -> Result<Vec<u8>, LinkError> {
                self.0.lock().unwrap().read_reply(timeout)
            }
            fn discard(&mut self, quiet: Duration) -> Result<Vec<u8>, LinkError> {
                self.0.lock().unwrap().discard(quiet)
            }
        }

        fn opener(link: FakeLink) -> (Opener, Arc<Mutex<FakeLink>>) {
            let shared = Arc::new(Mutex::new(link));
            let held = Arc::clone(&shared);
            let open: Opener =
                Box::new(move |_log| Ok(Box::new(Shared(held)) as Box<dyn Link + Send>));
            (open, shared)
        }

        fn failing(error: fn() -> LinkError) -> Opener {
            Box::new(move |_log| Err(error()))
        }

        /// Poll until the worker has said its last, and answer everything it
        /// said. Bounded, so a worker that never finishes fails the test
        /// rather than hanging it.
        fn to_the_end(w: &mut Worker) -> Vec<Event> {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut events = Vec::new();
            while !w.finished() {
                assert!(Instant::now() < deadline, "the worker never finished");
                events.extend(w.poll());
                std::thread::sleep(Duration::from_millis(1));
            }
            events.extend(w.poll());
            events
        }

        fn last(events: &[Event]) -> &Event {
            events.last().expect("the worker said nothing")
        }

        #[test]
        fn a_check_reports_the_firmware_and_status() {
            for (status, said) in [
                (
                    &b"0\x03"[..],
                    "Connected. Firmware CAMEO V1.10. The cutter is ready.",
                ),
                (
                    b"1\x03",
                    "Connected. Firmware CAMEO V1.10. The cutter is busy.",
                ),
                (
                    b"2\x03",
                    "Connected. Firmware CAMEO V1.10. No sheet is loaded.",
                ),
                (
                    b"7\x03",
                    "Connected. Firmware CAMEO V1.10. The cutter answered with a status \
                     Umber does not know: 7\\x03.",
                ),
            ] {
                let (open, link) = opener(FakeLink::new().statuses(&[status]));
                let mut w = Worker::start(Work::Check, open, fast(), || {});
                let events = to_the_end(&mut w);
                assert_eq!(last(&events), &Event::Checked(Ok(said.to_string())));
                // Only the three harmless commands went to the machine.
                assert_eq!(
                    link.lock().unwrap().writes,
                    vec![INIT.to_vec(), FIRMWARE.to_vec(), STATUS.to_vec()]
                );
            }
        }

        #[test]
        fn a_send_reports_each_stage_then_the_outcome() {
            let job = job(true);
            let n = job.path_blocks().len();
            let (open, _link) = opener(FakeLink::new());
            let mut w = Worker::start(Work::Send(job), open, fast(), || {});
            let events = to_the_end(&mut w);
            let stages: Vec<Stage> = events
                .iter()
                .filter_map(|e| match e {
                    Event::Stage(s) => Some(*s),
                    Event::Line(_) | Event::Checked(_) | Event::Sent(_) => None,
                })
                .collect();
            let mut expected = vec![Stage::Connecting, Stage::FindingMarks];
            expected.extend((0..n).map(|block| Stage::Cutting { block, of: n }));
            expected.push(Stage::Finishing);
            assert_eq!(stages, expected);
            assert_eq!(last(&events), &Event::Sent(Ok("Sent.".to_string())));
            // Exactly one final event, and it is the last thing said.
            let finals = events
                .iter()
                .filter(|e| matches!(e, Event::Sent(_) | Event::Checked(_)))
                .count();
            assert_eq!(finals, 1);
        }

        #[test]
        fn every_transfer_reaches_the_log() {
            let job = job(true);
            let (open, link) = opener(FakeLink::new());
            let mut w = Worker::start(Work::Send(job), open, fast(), || {});
            let events = to_the_end(&mut w);
            let lines: Vec<&str> = events
                .iter()
                .filter_map(|e| match e {
                    Event::Line(l) => Some(l.as_str()),
                    Event::Stage(_) | Event::Checked(_) | Event::Sent(_) => None,
                })
                .collect();
            let writes = link.lock().unwrap().writes.clone();
            let written: Vec<&str> = lines
                .iter()
                .copied()
                .filter(|l| l.starts_with("> "))
                .collect();
            assert_eq!(written.len(), writes.len(), "{lines:?}");
            for (line, bytes) in written.iter().zip(&writes) {
                assert_eq!(*line, log_line('>', bytes));
            }
            // A reply was read for every status query and the firmware, and
            // each of those is a line too.
            let read = lines.iter().filter(|l| l.starts_with("< ")).count();
            assert!(read > count(&writes, STATUS), "{lines:?}");
            assert_eq!(lines[0], "> 2 \\x1b\\x04");
        }

        #[test]
        fn a_worker_that_dies_is_noticed_once_and_wakes_the_loop() {
            for (work, said) in [
                (
                    Work::Send(job(false)),
                    Event::Sent(Err("The connection to the cutter stopped unexpectedly. \
                                     Check the sheet before sending again."
                        .to_string())),
                ),
                (
                    Work::Check,
                    Event::Checked(Err(
                        "The connection to the cutter stopped unexpectedly.".to_string()
                    )),
                ),
            ] {
                let wakes = Arc::new(AtomicUsize::new(0));
                let counter = Arc::clone(&wakes);
                let open: Opener = Box::new(|_log| panic!("an opener panicked"));
                let mut w = Worker::start(work, open, fast(), move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                });
                let deadline = Instant::now() + Duration::from_secs(10);
                while wakes.load(Ordering::SeqCst) == 0 {
                    assert!(Instant::now() < deadline, "a dead worker woke nothing");
                    std::thread::sleep(Duration::from_millis(1));
                }
                // The frame the wake asked for already sees the death: the
                // wake came after the channel went quiet.
                assert_eq!(w.poll(), vec![said]);
                assert!(w.finished());
                for _ in 0..5 {
                    assert_eq!(w.poll(), Vec::new(), "the death was reported twice");
                }
                assert_eq!(wakes.load(Ordering::SeqCst), 1);
            }
        }

        #[test]
        fn stop_lets_the_machine_finish_and_then_sends_it_home() {
            let job = job(true);
            let blocks = job.path_blocks();
            // Ready until the first block, then busy until the test says
            // otherwise: the job sits in the wait after block 0 until the stop
            // arrives, and goes on waiting after it.
            let mut fake = FakeLink::new().statuses(&[&b"0\x03"[..]; 3]);
            fake.idle = b"1\x03".to_vec();
            let (open, link) = opener(fake);
            let timing = Timing {
                ready: Duration::from_secs(10),
                ..fast()
            };
            let mut w = Worker::start(Work::Send(job), open, timing, || {});
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut events = Vec::new();
            while !events
                .iter()
                .any(|e| matches!(e, Event::Stage(Stage::Cutting { block: 0, .. })))
            {
                assert!(Instant::now() < deadline, "the first block never began");
                events.extend(w.poll());
                std::thread::sleep(Duration::from_millis(1));
            }
            w.stop();
            // The worker says it is stopping while the machine is still busy,
            // and does not end the job while it is.
            let deadline = Instant::now() + Duration::from_secs(10);
            while !events
                .iter()
                .any(|e| matches!(e, Event::Stage(Stage::Stopping)))
            {
                assert!(Instant::now() < deadline, "the stop was never said");
                events.extend(w.poll());
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(!w.finished(), "the job ended while the cutter was busy");
            link.lock().unwrap().idle = b"0\x03".to_vec();
            events.extend(to_the_end(&mut w));
            let stopped = Failure {
                error: SendError::Stopped,
                blocks_sent: 1,
                blocks: blocks.len(),
            }
            .sentence();
            assert_eq!(last(&events), &Event::Sent(Err(stopped)));
            let link = link.lock().unwrap();
            assert_eq!(count(&link.writes, &blocks[0]), 1);
            for b in &blocks[1..] {
                assert_eq!(count(&link.writes, b), 0);
            }
            sent_home_once_after_ready(&link, &super::job(true));
        }

        #[test]
        fn an_opener_that_fails_is_a_sentence() {
            let errors: [fn() -> LinkError; 3] = [
                || LinkError::NotFound,
                || LinkError::Busy,
                || LinkError::Unsupported,
            ];
            for checking in [false, true] {
                let mut said = Vec::new();
                for error in errors {
                    let work = if checking {
                        Work::Check
                    } else {
                        Work::Send(job(false))
                    };
                    let mut w = Worker::start(work, failing(error), fast(), || {});
                    let events = to_the_end(&mut w);
                    let sentence = match last(&events) {
                        Event::Sent(Err(s)) if !checking => s.clone(),
                        Event::Checked(Err(s)) if checking => s.clone(),
                        other => panic!("{other:?}"),
                    };
                    assert_eq!(sentence, SendError::Link(error()).sentence());
                    assert!(
                        !sentence.contains(['-', '\u{2013}', '\u{2014}']),
                        "{sentence}"
                    );
                    said.push(sentence);
                }
                for (i, s) in said.iter().enumerate() {
                    for t in &said[i + 1..] {
                        assert_ne!(s, t);
                    }
                }
            }
        }

        #[test]
        fn the_paths_found_are_logged_before_the_open() {
            assert_eq!(
                found_lines(&[]),
                vec!["No Silhouette cutter found on USB.".to_string()]
            );
            assert_eq!(
                found_lines(&["a".to_string(), "b".to_string()]),
                vec![
                    "Found a Silhouette at a".to_string(),
                    "Found a Silhouette at b".to_string()
                ]
            );
            // Whatever the opener logs reaches the log, ahead of any transfer.
            let open: Opener = Box::new(|log| {
                log("from the opener".to_string());
                Err(LinkError::NotFound)
            });
            let mut w = Worker::start(Work::Check, open, fast(), || {});
            let events = to_the_end(&mut w);
            assert_eq!(events[0], Event::Line("from the opener".to_string()));
        }
    }
}
