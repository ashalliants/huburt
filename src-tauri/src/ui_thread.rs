//! One dedicated thread for every window and web view operation (creating, closing, moving, showing, the tray).
//!
//! Why: on Windows, creating a window or web view, or reading cookies, deadlocks when it is done on the main thread, which is
//! where every synchronous command and event handler runs (Tauri documents this). So nothing on the main thread, and no
//! other thread either, calls those APIs directly: they post a job here instead. Posting never blocks. The jobs run one at a
//! time in the order they were posted, which also removes the need for a lock around the layout code, and the thread can
//! safely wait for the main thread because nothing the main thread does ever waits for this thread.
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Receiver, Sender},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

type Job = (&'static str, Box<dyn FnOnce() + Send>);

/// A job taking longer than this is logged (it is how a stuck web view creation would show up).
const SLOW: Duration = Duration::from_secs(5);

#[derive(Default)]
struct Status {
    waiting: AtomicUsize,
    running: Mutex<Option<(&'static str, Instant)>>,
}

pub struct UiThread {
    tx: Sender<Job>,
    status: Arc<Status>,
}

impl UiThread {
    pub fn start() -> UiThread {
        let (tx, rx) = mpsc::channel::<Job>();
        let status = Arc::new(Status::default());
        let worker_status = status.clone();
        std::thread::Builder::new()
            .name("huburt-ui".into())
            .spawn(move || run(rx, worker_status))
            .expect("start the UI thread");
        UiThread { tx, status }
    }

    /// Queues `job` to run on the UI thread, after everything already queued. Returns immediately.
    pub fn post(&self, name: &'static str, job: impl FnOnce() + Send + 'static) {
        self.status.waiting.fetch_add(1, Ordering::SeqCst);
        if self.tx.send((name, Box::new(job))).is_err() {
            self.status.waiting.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// What the thread is doing, for the log when something looks stuck.
    pub fn describe(&self) -> String {
        let waiting = self.status.waiting.load(Ordering::SeqCst);
        match *self.status.running.lock().unwrap_or_else(|e| e.into_inner()) {
            Some((name, since)) => format!("UI thread: running \"{name}\" for {:.1}s, {waiting} waiting", since.elapsed().as_secs_f32()),
            None => format!("UI thread: idle, {waiting} waiting"),
        }
    }
}

fn run(rx: Receiver<Job>, status: Arc<Status>) {
    for (name, job) in rx {
        status.waiting.fetch_sub(1, Ordering::SeqCst);
        let started = Instant::now();
        *status.running.lock().unwrap_or_else(|e| e.into_inner()) = Some((name, started));
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).is_err() {
            crate::log(&format!("ui job \"{name}\" panicked"));
        }
        *status.running.lock().unwrap_or_else(|e| e.into_inner()) = None;
        if started.elapsed() > SLOW {
            crate::log(&format!("ui job \"{name}\" took {:.1}s", started.elapsed().as_secs_f32()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jobs_run_in_order_on_another_thread_and_posting_never_blocks() {
        let ui = UiThread::start();
        let caller = std::thread::current().id();
        let (tx, rx) = mpsc::channel();
        // the first job is slow: posting the rest must still return at once
        let t = tx.clone();
        ui.post("slow", move || { std::thread::sleep(Duration::from_millis(150)); t.send((0, std::thread::current().id())).unwrap(); });
        let posted = Instant::now();
        for i in 1..=3 {
            let t = tx.clone();
            ui.post("next", move || t.send((i, std::thread::current().id())).unwrap());
        }
        assert!(posted.elapsed() < Duration::from_millis(100), "post() must not wait for jobs to run");
        let got: Vec<_> = (0..4).map(|_| rx.recv_timeout(Duration::from_secs(2)).unwrap()).collect();
        assert_eq!(got.iter().map(|g| g.0).collect::<Vec<_>>(), vec![0, 1, 2, 3], "in the order posted");
        assert!(got.iter().all(|g| g.1 != caller), "never on the posting thread");
    }

    #[test]
    fn a_job_can_post_more_jobs_without_deadlocking() {
        let ui = Arc::new(UiThread::start());
        let (tx, rx) = mpsc::channel();
        let inner = ui.clone();
        ui.post("outer", move || {
            inner.post("inner", move || tx.send("done").unwrap());
        });
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), "done");
    }

    #[test]
    fn describes_what_it_is_doing() {
        let ui = UiThread::start();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        ui.post("create service", move || { started_tx.send(()).unwrap(); let _ = release_rx.recv_timeout(Duration::from_secs(3)); });
        ui.post("layout", || {});
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let d = ui.describe();
        assert!(d.contains("running \"create service\"") && d.contains("1 waiting"), "{d}");
        release_tx.send(()).unwrap();
    }
}
