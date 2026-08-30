use std::collections::HashMap;

use crate::rollover::NativeThreadCursor;

const MAX_TOPIC_THREADS: usize = 8;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum InteractiveScope {
    Main,
    Topic(String),
}

impl InteractiveScope {
    pub fn topic_id(&self) -> Option<&str> {
        match self {
            Self::Main => None,
            Self::Topic(id) => Some(id),
        }
    }
}

#[derive(Debug)]
pub struct InteractiveThread {
    pub thread_id: String,
    pub cursor: NativeThreadCursor,
    last_used: u64,
}

#[derive(Debug)]
pub struct InteractiveThreads {
    main: InteractiveThread,
    topics: HashMap<String, InteractiveThread>,
    clock: u64,
}

impl InteractiveThreads {
    pub fn new(main_thread_id: String) -> Self {
        Self {
            main: InteractiveThread {
                thread_id: main_thread_id,
                cursor: NativeThreadCursor::new(),
                last_used: 0,
            },
            topics: HashMap::new(),
            clock: 0,
        }
    }

    pub fn contains(&self, scope: &InteractiveScope) -> bool {
        match scope {
            InteractiveScope::Main => true,
            InteractiveScope::Topic(id) => self.topics.contains_key(id),
        }
    }

    pub fn select(&mut self, scope: &InteractiveScope) -> &mut InteractiveThread {
        self.clock = self.clock.saturating_add(1);
        let thread = match scope {
            InteractiveScope::Main => &mut self.main,
            InteractiveScope::Topic(id) => self
                .topics
                .get_mut(id)
                .expect("topic thread must be inserted before selection"),
        };
        thread.last_used = self.clock;
        thread
    }

    pub fn insert_topic(&mut self, id: String, thread_id: String) -> Vec<String> {
        if let Some(existing) = self.topics.get_mut(&id) {
            let previous = std::mem::replace(&mut existing.thread_id, thread_id);
            existing.cursor.rotate();
            return vec![previous];
        }
        self.clock = self.clock.saturating_add(1);
        self.topics.insert(
            id,
            InteractiveThread {
                thread_id,
                cursor: NativeThreadCursor::new(),
                last_used: self.clock,
            },
        );
        if self.topics.len() <= MAX_TOPIC_THREADS {
            return Vec::new();
        }
        let evicted = self
            .topics
            .iter()
            .min_by_key(|(_, thread)| thread.last_used)
            .map(|(id, _)| id.clone())
            .expect("topic thread registry is non-empty");
        self.topics
            .remove(&evicted)
            .map(|thread| vec![thread.thread_id])
            .unwrap_or_default()
    }

    pub fn replace(&mut self, scope: &InteractiveScope, thread_id: String) -> String {
        let thread = self.select(scope);
        let previous = std::mem::replace(&mut thread.thread_id, thread_id);
        thread.cursor.rotate();
        previous
    }

    pub fn reset(&mut self, main_thread_id: String) -> Vec<String> {
        let mut previous = vec![std::mem::replace(&mut self.main.thread_id, main_thread_id)];
        self.main.cursor.rotate();
        previous.extend(self.topics.drain().map(|(_, thread)| thread.thread_id));
        previous
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_and_topic_cursors_advance_independently() {
        let mut threads = InteractiveThreads::new("main".to_owned());
        let topic = InteractiveScope::Topic("ep_topic".to_owned());
        threads.insert_topic("ep_topic".to_owned(), "topic".to_owned());

        let main = threads.select(&InteractiveScope::Main);
        main.cursor.bridge_completed();
        main.cursor.mark("rev_main".to_owned());
        let topic_thread = threads.select(&topic);
        topic_thread.cursor.bridge_completed();
        topic_thread.cursor.mark("rev_topic".to_owned());

        assert_eq!(
            threads.select(&InteractiveScope::Main).cursor.revision(),
            Some("rev_main")
        );
        assert_eq!(threads.select(&topic).cursor.revision(), Some("rev_topic"));
    }

    #[test]
    fn topic_thread_registry_evicts_the_least_recently_used_topic() {
        let mut threads = InteractiveThreads::new("main".to_owned());
        for index in 0..MAX_TOPIC_THREADS {
            threads.insert_topic(format!("ep_{index}"), format!("thread_{index}"));
        }
        threads.select(&InteractiveScope::Topic("ep_0".to_owned()));

        let evicted = threads.insert_topic("ep_next".to_owned(), "thread_next".to_owned());

        assert_eq!(evicted, vec!["thread_1"]);
        assert!(threads.contains(&InteractiveScope::Topic("ep_0".to_owned())));
        assert!(!threads.contains(&InteractiveScope::Topic("ep_1".to_owned())));
    }
}
