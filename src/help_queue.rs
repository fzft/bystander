use std::ptr::null_mut;
use std::sync::atomic::{AtomicIsize, AtomicPtr, Ordering};

use crate::{NormalizedLockFree, OperationRecordBox};

pub(crate) struct Node<T> {
    value: Option<T>,
    next: AtomicPtr<Self>,
    enq_id: Option<usize>,
    deq_id: AtomicIsize,
}

impl<T> Node<T> {
    fn new(value: T, enq_id: usize) -> *mut Self {
        Box::into_raw(Box::new(Self {
            value: Some(value),
            next: AtomicPtr::new(null_mut()),
            enq_id: Some(enq_id),
            deq_id: AtomicIsize::new(-1),
        }))
    }

    fn sentinel() -> Self {
        Self {
            value: None,
            next: AtomicPtr::new(null_mut()),
            enq_id: None,
            deq_id: AtomicIsize::new(-1),
        }
    }
}

pub(crate) struct OpDesc<T> {
    phase: Option<u64>,
    pending: bool,
    enqueue: bool,
    node: Option<*mut Node<T>>,
}

pub struct WaitFreeHelpQueue<T, const N: usize> {
    head: AtomicPtr<Node<T>>,
    tail: AtomicPtr<Node<T>>,
    state: [AtomicPtr<OpDesc<T>>; N],
}

impl<T, const N: usize> WaitFreeHelpQueue<T, N> where T: Copy + PartialEq + Eq {
    pub fn new() -> Self {
        let sentinel = Box::into_raw(Box::new(Node::sentinel()));
        let head = AtomicPtr::new(sentinel);
        let tail = AtomicPtr::new(sentinel);
        let state: [AtomicPtr<OpDesc<T>>; N] = (0..N).map(|_| {
            AtomicPtr::new(Box::into_raw(Box::new(
                OpDesc {
                    phase: None,
                    pending: false,
                    enqueue: true,
                    node: None,
                }
            )))
        }).collect::<Vec<_>>().try_into().expect("give N elements");
        Self {
            head,
            tail,
            state,
        }
    }

    fn max_phase(&self) -> Option<u64> {
        self.state.iter().filter_map(|s| {
            unsafe { &*s.load(Ordering::SeqCst) }.phase
        }).max()
    }

    fn is_still_pending(&self, id: usize, phase: u64) -> bool {
        let state = unsafe { &*self.state[id].load(Ordering::SeqCst) };
        state.pending && state.phase.unwrap_or(0) <= phase
    }

    fn help(&self, phase: u64) {
        for (id,  desc_atomic) in self.state.iter().enumerate() {
            let desc_ptr = desc_atomic.load(Ordering::SeqCst);
            let desc = unsafe{&*desc_ptr};
            if desc.pending && desc.phase.unwrap_or(0) <= phase {
                if desc.enqueue {
                    self.help_enq(id, phase);
                }
            }
        }
    }

    fn help_finish_enq(&self) {
        let last_ptr = self.tail.load(Ordering::SeqCst);
        let last = unsafe { &*last_ptr };
        let next_ptr = last.next.load(Ordering::SeqCst);
        if next_ptr.is_null() {
            return;
        }
        let next = unsafe { &*next_ptr };
        let id = next.enq_id.expect("next is never the sentinel");
        let cur_desc_ptr = self.state[id].load(Ordering::SeqCst);
        let cur_desc = unsafe { &*cur_desc_ptr };
        if last_ptr == self.tail.load(Ordering::SeqCst) {
            return;
        }
        if cur_desc.node.unwrap_or(null_mut()) != next_ptr {
            return;
        }

        let new_desc_ptr = Box::into_raw(Box::new(OpDesc {
            phase: cur_desc.phase,
            pending: false,
            enqueue: true,
            node: cur_desc.node,
        }));

        let _ = self.state[id].compare_exchange(cur_desc_ptr, new_desc_ptr, Ordering::SeqCst, Ordering::Relaxed);
        let _ = self.tail.compare_exchange(last_ptr, next_ptr, Ordering::SeqCst, Ordering::Relaxed);
    }

    pub(crate) fn enqueue(&self, id: usize, value: T) {
        let phase = self.max_phase().map_or(0, |p| p + 1);
        self.state[id].store(Box::into_raw(Box::new(OpDesc {
            phase: Some(phase),
            pending: true,
            enqueue: true,
            node: Some(Node::new(value, id)),
        })), Ordering::SeqCst);
        self.help(phase);
        self.help_finish_enq();
    }

    fn help_enq(&self, id: usize, phase: u64) {
        while self.is_still_pending(id, phase) {
            let last_ptr = self.tail.load(Ordering::SeqCst);
            let last = unsafe { &*last_ptr };
            let next_ptr = last.next.load(Ordering::SeqCst);
            if last_ptr != self.tail.load(Ordering::SeqCst) {
                continue;
            }
            if !next_ptr.is_null() {
                self.help_finish_enq();
                continue;
            }
            if !self.is_still_pending(id, phase) {
                continue
            }

            let curr_desc_ptr = self.state[id].load(Ordering::SeqCst);
            let curr_desc = unsafe {&*curr_desc_ptr};

            if !curr_desc.pending {
                continue;
            }

            debug_assert!(curr_desc.enqueue);
            if last.next.compare_exchange(next_ptr, curr_desc.node.expect("node should always be some for pending enqueue"), Ordering::SeqCst, Ordering::Relaxed).is_ok() {
                self.help_finish_enq();
                return;
            }

        }
    }

    pub(crate) fn peek(&self) -> Option<T> {
        let node = unsafe { &*self.head.load(Ordering::SeqCst) };
        let next = node.next.load(Ordering::SeqCst);
        if next.is_null() {
            None
        } else {
            Some(unsafe { &*next }.value.expect("not a sentinel node"))
        }
    }

    pub(crate) fn try_remove_front(&self, front: T) -> Result<(), ()> {
        let curr_head_ptr = self.head.load(Ordering::SeqCst);
        let curr_head = unsafe { &*curr_head_ptr };
        let next = curr_head.next.load(Ordering::SeqCst);
        if next.is_null() || unsafe { &*next }.value.as_ref().expect("not a sentinel node") as *const _ != &front {
            return Err(());
        }

        match self.head.compare_exchange(curr_head_ptr, next, Ordering::SeqCst, Ordering::Relaxed) {
            Ok(_) => {
                self.help_finish_enq();
                curr_head.next.store(null_mut(), Ordering::SeqCst);
                Ok(())
            }
            Err(_) => Err(())
        }
    }
}

// A wait-free queue
pub type HelpQueue<LF: NormalizedLockFree, const N: usize> = WaitFreeHelpQueue<*const OperationRecordBox<LF>, N>;

