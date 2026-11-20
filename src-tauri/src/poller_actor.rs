//! Each unread poller (Slack, Gmail) lives on its own dedicated thread and is driven by messages, in the order they are sent.
//!
//! Why: a poller does blocking network requests that can take many seconds. If other code had to lock a shared poller, a slow
//! request would stall whoever wanted it, and whoever that was (the core, on the main thread, answering a click) would freeze
//! the app. With an actor, sending an operation never blocks, however busy the poller is, and because one thread does the
//! operations one at a time in the order they were sent, "reset, then poll" and "hand over credentials, then poll" can't be
//! reordered, and the state each operation reports comes back in that same order, so a stale report can never overwrite a
//! newer one.
use crate::types::{Credentials, PollResult, Poller};
use std::{
    collections::HashMap,
    sync::mpsc::{self, Sender},
};

/// What an operation tells the app about the poller afterwards.
#[derive(Debug, Clone, PartialEq)]
pub enum Report {
    /// A poll finished.
    Polled { result: Option<PollResult>, signed_out: bool, needs_creds: bool },
    /// Credentials were handed over.
    Credentials { needs_creds: bool },
}

enum Op {
    Poll,
    Reset,
    SetOption(&'static str, String),
    Credentials(Credentials),
    KeepOnline(Box<dyn FnOnce(bool) + Send>),
    Perform { action: String, info: HashMap<String, String>, text: Option<String>, done: Box<dyn FnOnce(bool) + Send> },
}

/// A handle to a poller's thread. Cloning is cheap, and every method returns immediately.
#[derive(Clone)]
pub struct PollerHandle {
    tx: Sender<Op>,
}

impl PollerHandle {
    /// Starts the poller's thread. `report` is called on that thread, after each operation, in operation order.
    pub fn spawn(id: &'static str, mut poller: Box<dyn Poller>, report: impl Fn(&'static str, Report) + Send + 'static) -> PollerHandle {
        let (tx, rx) = mpsc::channel::<Op>();
        std::thread::Builder::new()
            .name(format!("huburt-poller-{id}"))
            .spawn(move || {
                for op in rx {
                    match op {
                        Op::Poll => {
                            let result = poller.poll();
                            report(id, Report::Polled { result, signed_out: poller.signed_out(), needs_creds: poller.needs_credentials() });
                        }
                        Op::Reset => poller.reset(),
                        Op::SetOption(key, value) => poller.set_option(key, &value),
                        Op::Credentials(creds) => {
                            poller.set_credentials(creds);
                            report(id, Report::Credentials { needs_creds: poller.needs_credentials() });
                        }
                        Op::KeepOnline(done) => done(poller.keep_online()),
                        Op::Perform { action, info, text, done } => done(poller.perform(&action, &info, text.as_deref())),
                    }
                }
            })
            .expect("start a poller thread");
        PollerHandle { tx }
    }

    /// Check for unread items now; the answer arrives as `Report::Polled`.
    pub fn poll(&self) { let _ = self.tx.send(Op::Poll); }
    /// Forget what has been seen, so the next poll only records the starting point.
    pub fn reset(&self) { let _ = self.tx.send(Op::Reset); }
    pub fn set_option(&self, key: &'static str, value: &str) { let _ = self.tx.send(Op::SetOption(key, value.to_string())); }
    pub fn credentials(&self, creds: Credentials) { let _ = self.tx.send(Op::Credentials(creds)); }
    /// Slack "show as online"; `done(true)` if it was sent.
    pub fn keep_online(&self, done: impl FnOnce(bool) + Send + 'static) { let _ = self.tx.send(Op::KeepOnline(Box::new(done))); }
    /// A banner button (reply, mark as read); `done(true)` if it worked.
    pub fn perform(&self, action: String, info: HashMap<String, String>, text: Option<String>, done: impl FnOnce(bool) + Send + 'static) {
        let _ = self.tx.send(Op::Perform { action, info, text, done: Box::new(done) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{Arc, Mutex},
        time::{Duration, Instant},
    };

    type Calls = Arc<Mutex<Vec<String>>>;

    /// Records what it is asked to do, in order. `needs` starts true and credentials clear it, like the real ones.
    struct Recorder {
        calls: Calls,
        needs: bool,
        poll_takes: Duration,
    }
    impl Poller for Recorder {
        fn poll(&mut self) -> Option<PollResult> {
            std::thread::sleep(self.poll_takes);
            self.calls.lock().unwrap().push("poll".into());
            None
        }
        fn needs_credentials(&self) -> bool { self.needs }
        fn signed_out(&self) -> bool { false }
        fn set_credentials(&mut self, _: Credentials) { self.calls.lock().unwrap().push("credentials".into()); self.needs = false; }
        fn reset(&mut self) { self.calls.lock().unwrap().push("reset".into()); }
        fn set_option(&mut self, key: &str, value: &str) { self.calls.lock().unwrap().push(format!("option {key}={value}")); }
        fn perform(&mut self, action: &str, _: &HashMap<String, String>, text: Option<&str>, ) -> bool {
            self.calls.lock().unwrap().push(format!("perform {action} {}", text.unwrap_or("")));
            true
        }
        fn keep_online(&mut self) -> bool { self.calls.lock().unwrap().push("keep_online".into()); true }
    }

    fn start(poll_takes: Duration) -> (PollerHandle, Calls, mpsc::Receiver<Report>) {
        let calls: Calls = Arc::default();
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let handle = PollerHandle::spawn("test", Box::new(Recorder { calls: calls.clone(), needs: true, poll_takes }), move |_, r| {
            let _ = tx.lock().unwrap().send(r);
        });
        (handle, calls, rx)
    }

    fn wait_for(calls: &Calls, n: usize) {
        let until = Instant::now() + Duration::from_secs(3);
        while calls.lock().unwrap().len() < n && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn operations_run_in_the_order_they_were_sent() {
        let (p, calls, _rx) = start(Duration::ZERO);
        p.reset();                                     // e.g. a service being turned back on...
        p.poll();                                      // ...and the poll that follows must see the reset
        p.set_option("source", "all");
        p.credentials(Credentials::default());
        p.poll();
        wait_for(&calls, 5);
        assert_eq!(*calls.lock().unwrap(), vec!["reset", "poll", "option source=all", "credentials", "poll"]);
    }

    #[test]
    fn a_report_is_never_overtaken_by_an_older_one() {
        // A poll that was started while credentials were still missing, followed by the credentials arriving: the poll's
        // report ("still needs credentials") must be delivered BEFORE the credentials' ("has them"), so applying reports in
        // order ends on the newer state. (Sampling the state later, under another lock, could do the opposite.)
        let (p, _calls, rx) = start(Duration::from_millis(100));
        p.poll();
        p.credentials(Credentials::default());
        let first = rx.recv_timeout(Duration::from_secs(3)).unwrap();
        let second = rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(first, Report::Polled { result: None, signed_out: false, needs_creds: true });
        assert_eq!(second, Report::Credentials { needs_creds: false });
    }

    #[test]
    fn sending_never_waits_for_a_busy_poller() {
        let (p, _calls, _rx) = start(Duration::from_millis(500));
        p.poll();                                      // busy for half a second
        let started = Instant::now();
        p.reset();
        p.set_option("source", "important");
        p.credentials(Credentials::default());
        p.poll();
        assert!(started.elapsed() < Duration::from_millis(100), "sending must return at once");
    }

    #[test]
    fn button_presses_and_presence_report_back_after_everything_queued_before_them() {
        let (p, calls, _rx) = start(Duration::ZERO);
        let (tx, rx) = mpsc::channel();
        p.reset();
        let t = tx.clone();
        p.perform("reply".into(), HashMap::new(), Some("thanks".into()), move |ok| t.send(("perform", ok)).unwrap());
        p.keep_online(move |ok| tx.send(("online", ok)).unwrap());
        assert_eq!(rx.recv_timeout(Duration::from_secs(3)).unwrap(), ("perform", true));
        assert_eq!(rx.recv_timeout(Duration::from_secs(3)).unwrap(), ("online", true));
        assert_eq!(*calls.lock().unwrap(), vec!["reset", "perform reply thanks", "keep_online"]);
    }
}
