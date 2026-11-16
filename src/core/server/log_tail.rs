#![cfg_attr(not(test), allow(dead_code))]

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) struct LogTail {
    lines: VecDeque<String>,
    limit: usize,
    open: Arc<AtomicBool>,
}

pub(crate) struct LogReader {
    open: Arc<AtomicBool>,
}

impl LogTail {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            limit: limit.max(1),
            open: Arc::new(AtomicBool::new(true)),
        }
    }

    pub(crate) fn reader(&self) -> LogReader {
        LogReader {
            open: Arc::clone(&self.open),
        }
    }

    pub(crate) fn push(&mut self, line: &str) -> bool {
        if !self.open.load(Ordering::Acquire) {
            return false;
        }
        if self.lines.len() == self.limit {
            self.lines.pop_front();
        }
        self.lines.push_back(line.to_string());
        true
    }

    pub(crate) fn lines(&self) -> impl Iterator<Item = &str> {
        self.lines.iter().map(String::as_str)
    }
}

impl Drop for LogReader {
    fn drop(&mut self) {
        self.open.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropping_the_reader_stops_the_tail() {
        let mut tail = LogTail::new(2);
        let reader = tail.reader();
        assert!(tail.push("one"));
        assert!(tail.push("two"));
        assert!(tail.push("three"));
        assert_eq!(tail.lines().collect::<Vec<_>>(), vec!["two", "three"]);
        drop(reader);
        assert!(!tail.push("four"));
        assert_eq!(tail.lines().collect::<Vec<_>>(), vec!["two", "three"]);
    }
}
