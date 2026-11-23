//! The scheduler's bookkeeping: which live tasks a sweep has to look at.
//!
//! A sweep polls every task that could run, in spawn order — that order is
//! the interleaving §12 specifies, and nothing here changes it. What this
//! changes is the cost of the tasks that could *not* run. They used to be a
//! flag on each entry of one list, so every sweep stepped over all of them,
//! and every completion woke every parked task to be polled again. In a chain
//! of awaits only the innermost task can run, and its parent is earlier in
//! spawn order: each sweep finished one link and stepped over, or re-polled,
//! all the rest. A recursive walk ten thousand deep took seconds.
//!
//! So a task waiting on something is filed under what it waits on, and a
//! sweep walks only the ones that are not: a task awaiting another wakes when
//! *that* task finishes. The polls this leaves out are the ones that found the
//! awaited task unfinished and parked again; they ran no code of the program,
//! so leaving them out changes nothing it can observe — provided the ones that
//! *would* have found it finished still happen, at the same place. Under the
//! old rule an awaiter woke at the end of every sweep in which anything
//! finished, and was polled at its own place in the next. So it wakes at the
//! end of the sweep its task finished in; and in a sweep that follows one in
//! which anything finished — an *armed* sweep — an awaiter later in spawn
//! order than its task, when that task finishes, is polled in the same sweep,
//! as the old rule's poll at its place would have found the task finished.
//! `task.park()`, which says only "some other task", is still woken by every
//! completion, and so is every deadline.
//!
//! A copy of the VM's `sched.rs`, as the rest of this scheduler is a copy of
//! the VM's: the two must poll in the same order, so change them together.
//! Below this comment the two are one text, and `tests` fails when they are not.

use std::collections::{BTreeMap, BTreeSet, HashMap};

/// One live task: how to resume it, and what it is waiting for.
pub struct Scheduled<P> {
    pub poll: P,
    /// The earliest clock reading at which polling could achieve anything.
    pub wake_at: Option<i64>,
    /// `task.park()`: waiting for some other task to finish.
    parked: bool,
    /// An `await`: waiting for this task, by number, to finish.
    awaiting: Option<u64>,
    /// Waiting on the host rather than on anything in the program.
    on_host: bool,
}

impl<P> Scheduled<P> {
    fn runnable(&self) -> bool {
        !self.parked && self.awaiting.is_none() && !self.on_host
    }
}

/// What a task asked for during the poll that just returned.
#[derive(Default)]
pub struct Requests {
    pub wake_at: Option<i64>,
    pub park: bool,
    pub park_on: Option<u64>,
    pub host: bool,
}

pub struct Tasks<P> {
    /// Every live task, by number: the order they were spawned in.
    all: BTreeMap<u64, Scheduled<P>>,
    /// The ones waiting on neither another task nor the host. A sleeping
    /// task is here too, and a sweep checks its deadline.
    ready: BTreeSet<u64>,
    parked: BTreeSet<u64>,
    on_host: BTreeSet<u64>,
    /// The ones with a deadline.
    timed: BTreeSet<u64>,
    /// Who awaits each task.
    waiters: HashMap<u64, Vec<u64>>,
    next: u64,
    /// Whether something finished in the last sweep: see the module's note.
    armed: bool,
}

impl<P> Default for Tasks<P> {
    fn default() -> Self {
        Tasks {
            all: BTreeMap::new(),
            ready: BTreeSet::new(),
            parked: BTreeSet::new(),
            on_host: BTreeSet::new(),
            timed: BTreeSet::new(),
            waiters: HashMap::new(),
            next: 0,
            armed: false,
        }
    }
}

impl<P> Tasks<P> {
    pub fn is_empty(&self) -> bool {
        self.all.is_empty()
    }

    pub fn len(&self) -> usize {
        self.all.len()
    }

    /// A new task, runnable, and later in spawn order than every other: a
    /// sweep that is running reaches it before it ends. Answers its number.
    pub fn spawn(&mut self, poll: P) -> u64 {
        let id = self.next;
        self.next += 1;
        self.all.insert(
            id,
            Scheduled { poll, wake_at: None, parked: false, awaiting: None, on_host: false },
        );
        self.ready.insert(id);
        id
    }

    /// The first task a sweep should look at whose number is `from` or more.
    pub fn next_ready(&self, from: u64) -> Option<u64> {
        self.ready.range(from..).next().copied()
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut Scheduled<P>> {
        self.all.get_mut(&id)
    }

    /// Every task's resume closure, for a collector to trace.
    // One of the two runtimes never calls this; the copies stay one text.
    #[allow(dead_code)]
    pub fn polls_mut(&mut self) -> impl Iterator<Item = &mut P> {
        self.all.values_mut().map(|t| &mut t.poll)
    }

    /// The task is about to be polled: whatever deadline it had is spent.
    pub fn start(&mut self, id: u64) {
        if let Some(t) = self.all.get_mut(&id) {
            t.wake_at = None;
        }
        self.timed.remove(&id);
    }

    /// The poll returned without finishing: file the task under what it
    /// asked to wait for.
    pub fn file(&mut self, id: u64, r: Requests) {
        // Awaiting a task that is not live is waiting on nothing that will
        // ever say so; treat it as the plain park it used to be.
        let (park, awaiting) = match r.park_on {
            Some(on) if self.all.contains_key(&on) => (r.park, Some(on)),
            Some(_) => (true, None),
            None => (r.park, None),
        };
        let Some(t) = self.all.get_mut(&id) else { return };
        t.wake_at = r.wake_at;
        t.parked = park;
        t.awaiting = awaiting;
        t.on_host = r.host;
        let runnable = t.runnable();
        if r.wake_at.is_some() {
            self.timed.insert(id);
        }
        if park {
            self.parked.insert(id);
        }
        if let Some(on) = awaiting {
            self.waiters.entry(on).or_default().push(id);
        }
        if r.host {
            self.on_host.insert(id);
        }
        if !runnable {
            self.ready.remove(&id);
        }
    }

    /// The poll returned finished: forget the task. In an armed sweep, an
    /// awaiter later in spawn order runs in this sweep still; answer the rest,
    /// to wake when it ends.
    pub fn finish(&mut self, id: u64) -> Vec<u64> {
        self.all.remove(&id);
        self.ready.remove(&id);
        self.timed.remove(&id);
        let mut later = Vec::new();
        for w in self.waiters.remove(&id).unwrap_or_default() {
            if self.armed && w > id {
                if let Some(t) = self.all.get_mut(&w) {
                    t.awaiting = None;
                }
                self.release(w);
            } else {
                later.push(w);
            }
        }
        later
    }

    /// The end of a sweep. If something finished in it, the tasks that
    /// awaited what finished, every parked task and every sleeping one get
    /// another look, as they did when a completion woke everything.
    pub fn swept(&mut self, completed: bool, awaiters: Vec<u64>) {
        self.armed = completed;
        if !completed {
            return;
        }
        for id in awaiters {
            if let Some(t) = self.all.get_mut(&id) {
                t.awaiting = None;
            }
            self.release(id);
        }
        for id in std::mem::take(&mut self.parked) {
            if let Some(t) = self.all.get_mut(&id) {
                t.parked = false;
            }
            self.release(id);
        }
        for id in std::mem::take(&mut self.timed) {
            if let Some(t) = self.all.get_mut(&id) {
                t.wake_at = None;
            }
        }
    }

    // One of the two runtimes never calls this; the copies stay one text.
    #[allow(dead_code)]
    pub fn any_on_host(&self) -> bool {
        !self.on_host.is_empty()
    }

    /// The host has had its turn: every task waiting on it gets another look.
    pub fn host_turned(&mut self) {
        for id in std::mem::take(&mut self.on_host) {
            if let Some(t) = self.all.get_mut(&id) {
                t.on_host = false;
            }
            self.release(id);
        }
    }

    /// The earliest deadline any task has.
    pub fn next_deadline(&self) -> Option<i64> {
        self.timed.iter().filter_map(|id| self.all.get(id)?.wake_at).min()
    }

    fn release(&mut self, id: u64) {
        if self.all.get(&id).is_some_and(|t| t.runnable()) {
            self.ready.insert(id);
        }
    }
}
