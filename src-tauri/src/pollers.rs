//! Builds the background poller for a service (the ones that can check unread counts without the page loaded).
use crate::{gmail::GmailPoller, slack::SlackPoller, types::Poller};

pub fn make(id: &str) -> Option<Box<dyn Poller>> {
    match id {
        "slack" => Some(Box::new(SlackPoller::new())),
        "gmail" => Some(Box::new(GmailPoller::new())),
        _ => None,
    }
}
