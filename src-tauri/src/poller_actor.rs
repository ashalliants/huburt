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
    /// A poll finished. `generation` is the one the poll was requested under (see `PollerHandle::poll`).
    Polled { generation: u64, result: Option<PollResult>, signed_out: bool, needs_creds: bool },
    /// Credentials were handed over.
    Credentials { needs_creds: bool },
}

enum Op {
    Poll(u64),
    /// Changing a poller's options and making it forget what it has seen, as ONE message, so nothing can be handled between
    /// the parts.
    Reconfigure { option: Option<(&'static str, String)>, reset: bool },
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
                        Op::Poll(generation) => {
                            let result = poller.poll();
                            report(id, Report::Polled { generation, result, signed_out: poller.signed_out(), needs_creds: poller.needs_credentials() });
                        }
                        Op::Reconfigure { option, reset } => {
                            if let Some((key, value)) = option { poller.set_option(key, &value); }
                            if reset { poller.reset(); }
                        }
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

    /// Check for unread items now; the answer arrives as `Report::Polled`, carrying `generation`. The app bumps the
    /// generation whenever it reconfigures the poller and ignores answers from older ones, which were computed with the old
    /// options or the old "what has been seen".
    pub fn poll(&self, generation: u64) { let _ = self.tx.send(Op::Poll(generation)); }
    /// Set an option and/or forget what has been seen (so the next poll only records the starting point), atomically.
    pub fn reconfigure(&self, option: Option<(&'static str, &str)>, reset: bool) {
        let _ = self.tx.send(Op::Reconfigure { option: option.map(|(k, v)| (k, v.to_string())), reset });
    }
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
        p.reconfigure(None, true);                     // e.g. a service being turned back on...
        p.poll(1);                                     // ...and the poll that follows must see the reset
        p.reconfigure(Some(("source", "all")), false);
        p.credentials(Credentials::default());
        p.poll(2);
        wait_for(&calls, 5);
        assert_eq!(*calls.lock().unwrap(), vec!["reset", "poll", "option source=all", "credentials", "poll"]);
    }

    #[test]
    fn a_report_is_never_overtaken_by_an_older_one() {
        // A poll that was started while credentials were still missing, followed by the credentials arriving: the poll's
        // report ("still needs credentials") must be delivered BEFORE the credentials' ("has them"), so applying reports in
        // order ends on the newer state. (Sampling the state later, under another lock, could do the opposite.)
        let (p, _calls, rx) = start(Duration::from_millis(100));
        p.poll(7);
        p.credentials(Credentials::default());
        let first = rx.recv_timeout(Duration::from_secs(3)).unwrap();
        let second = rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(first, Report::Polled { generation: 7, result: None, signed_out: false, needs_creds: true });
        assert_eq!(second, Report::Credentials { needs_creds: false });
    }

    #[test]
    fn a_reconfiguration_is_one_message_that_nothing_can_slip_into() {
        // Another thread (the one that refreshes cookies and then polls) can be sending polls at the same moment the app
        // reconfigures the poller. Option and reset must still be handled back to back: a poll between them would run with
        // the new option but the old "what has been seen".
        let (p, calls, _rx) = start(Duration::ZERO);
        let spammer = {
            let p = p.clone();
            std::thread::spawn(move || { for i in 0..300 { p.poll(i); } })
        };
        for _ in 0..50 {
            p.reconfigure(Some(("source", "all")), true);
        }
        spammer.join().unwrap();
        wait_for(&calls, 350);
        let log = calls.lock().unwrap().clone();
        assert_eq!(log.iter().filter(|c| c.starts_with("option")).count(), 50);
        for (i, call) in log.iter().enumerate() {
            if call.starts_with("option") {
                assert_eq!(log.get(i + 1).map(String::as_str), Some("reset"), "a poll slipped between option and reset at {i}");
            }
        }
    }

    #[test]
    fn sending_never_waits_for_a_busy_poller() {
        let (p, _calls, _rx) = start(Duration::from_millis(500));
        p.poll(1);                                     // busy for half a second
        let started = Instant::now();
        p.reconfigure(Some(("source", "important")), true);
        p.credentials(Credentials::default());
        p.poll(2);
        assert!(started.elapsed() < Duration::from_millis(100), "sending must return at once");
    }

    #[test]
    fn button_presses_and_presence_report_back_after_everything_queued_before_them() {
        let (p, calls, _rx) = start(Duration::ZERO);
        let (tx, rx) = mpsc::channel();
        p.reconfigure(None, true);
        let t = tx.clone();
        p.perform("reply".into(), HashMap::new(), Some("thanks".into()), move |ok| t.send(("perform", ok)).unwrap());
        p.keep_online(move |ok| tx.send(("online", ok)).unwrap());
        assert_eq!(rx.recv_timeout(Duration::from_secs(3)).unwrap(), ("perform", true));
        assert_eq!(rx.recv_timeout(Duration::from_secs(3)).unwrap(), ("online", true));
        assert_eq!(*calls.lock().unwrap(), vec!["reset", "perform reply thanks", "keep_online"]);
    }
}
