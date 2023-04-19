use std::intrinsics::atomic_and_release;
use std::marker::PhantomData;
use std::mem::transmute;
use std::ops::Index;
use std::slice::SliceIndex;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU8, AtomicUsize, Ordering};

const CONTENTION_THRESHOLD: usize = 2;
const RETRY_THRESHOLD: usize = 2;

pub struct Contention;

pub struct ContentionMeasure(usize);

impl ContentionMeasure {
    pub fn detected(&mut self) -> Result<(), Contention> {
        self.0 += 1;
        if self.0 > CONTENTION_THRESHOLD {
            Ok(())
        } else {
            Err(Contention)
        }
    }

    pub fn use_slow_path(&self) -> bool {
        self.0 > CONTENTION_THRESHOLD
    }
}

#[repr(u8)]
enum CasState {
    Success,
    Failure,
    Pending,
}

pub struct CasByRcu<T, M> {
    version: u64,
    value: T,
    meta: M,
}

pub struct Atomic<T, M>(AtomicPtr<CasByRcu<T, M>>);

pub trait VersionedCas {
    fn execute(&self) -> bool;
    fn has_modified_bit(&self) -> bool;
    fn clear_bit(&self) -> bool;
    fn state(&self) -> CasState;
    fn set_state(&self, new: &CasState);
}

impl<T, M> Atomic<T, M>
    where M: PartialEq + Eq
{
    pub fn new(initial: T, meta: M) -> Self {
        Self(AtomicPtr::new(Box::into_raw(Box::new(CasByRcu {
            version: 0,
            meta,
            value: initial,
        }))))
    }

    pub fn get2(&self) -> (&T, &M, u64) {
        let this_ptr = self.get();
        let this = unsafe { &*this_ptr };
        (&this.value, &this.meta, this.version)

    }
    pub fn with<F, R>(&self, f: F) -> R where F: FnOnce(&T, &M) -> R {
        let this_ptr = self.get();
        let this = unsafe { &*this_ptr };
        f(&this.value, &this.meta)

    }

    fn get(&self) -> *const CasByRcu<T, M> {
        self.0.load(Ordering::SeqCst)
    }


    pub fn value(&self) -> &T {
        // Safety: this is safe because we never deallocate
        &unsafe { &*self.0.load(Ordering::SeqCst) }.value
    }

    pub fn meta(&self) -> &M {
        // Safety: this is safe because we never deallocate
        &unsafe { &*self.0.load(Ordering::SeqCst) }.meta
    }

    fn compare_and_set(&self, expected: &T, expected_meta: &M, new: T, new_meta: M) -> bool {
        let this_ptr = self.0.load(Ordering::SeqCst);
        let this = unsafe { &*this_ptr };
        if this.value == expected && &this.meta == expected_meta {
            if expected != new {
                let _ = self.0.compare_exchange_weak(this_ptr,
                                                     Box::into_raw(
                                                         Box::new(CasByRcu {
                                                             version: this.version + 1,
                                                             meta: new_meta,
                                                             value: new,
                                                         })),
                                                     Ordering::SeqCst, Ordering::SeqCst);
                true
            }
        }
        false
    }
}

impl<T> VersionAtomic for Atomic<T> {
    fn execute(&self) -> bool {
        todo!()
    }

    fn has_modified_bit(&self) -> bool {
        todo!()
    }

    fn clear_bit(&self) -> bool {
        todo!()
    }

    fn state(&self) -> CasState {
        todo!()
    }

    fn set_state(&self, new: &CasState) {
        todo!()
    }
}

// in bystander
pub trait NormalizedLockFree {
    type Cas: CasDescriptor;
    type CommitDescriptor: CasDescriptors<Self::Cas> + Clone;
    type Output: Clone;
    type Input: Clone;


    fn generator(&self, op: &Self::Input, contention: &mut ContentionMeasure) -> Result<Self::CommitDescriptor, Contention>;
    fn wrap_up(&self, executed: Result<(), usize>, performed: &Self::CommitDescriptor, contention: &mut ContentionMeasure) -> Result<Option<Self::Output>, Contention>;
}

struct OperationRecordBox<LF: NormalizedLockFree> {
    val: AtomicPtr<OperationRecord<LF>>,
}

enum OperationState<LF: NormalizedLockFree> {
    PreCas,
    ExecuteCas(LF::CommitDescriptor),
    PostCas(LF::CommitDescriptor, Result<(), usize>),
    Completed(LF::Output),
}


struct OperationRecord<LF: NormalizedLockFree> {
    owner: std::thread::ThreadId,
    state: OperationState<LF>,
    input: LF::Input,
}


// A wait-free queue
pub struct HelpQueue<LF> {
    _o: PhantomData<LF>,
}

impl<LF: NormalizedLockFree> HelpQueue<LF> {
    fn enqueue(&self, help: *const OperationRecordBox<LF>) {}

    fn peek(&self) -> Option<*const OperationRecordBox<LF>> {
        None
    }

    fn try_remove_front(&self, completed: *const OperationRecordBox<LF>) -> Result<(), ()> {
        Err(())
    }
}

pub struct WaitFreeSimulator<LF: NormalizedLockFree> {
    algorithm: LF,
    help: HelpQueue<LF>,
}


impl<LF: NormalizedLockFree> WaitFreeSimulator<LF>
    where
            for<'a> &'a LF::CommitDescriptor: IntoIterator<Item=&'a dyn VersionAtomic>
{
    fn cas_executor(&self, descriptors: &LF::CommitDescriptor, content: &mut ContentionMeasure) -> Result<(), usize> {
        let len = descriptors.len();
        for (i, cas) in descriptors.into_iter().enumerate() {
            // TODO: Check if already complete
            if cas.execute().is_err() {
                content.detected();
                return Err(i);
            }
        }
        Ok(())
    }

    fn help_first(&self) {
        if let Some(help) = self.help.peek() {
            self.help_op(unsafe { &*help })
        }
    }

    // Guarantees that on return, orb is no longer in help queue
    fn help_op(&self, orb: &OperationRecordBox<LF>) {
        loop {
            let or = unsafe { &*orb.val.load(Ordering::SeqCst) };
            let updated_or = match &or.state {
                OperationState::Completed(..) => {
                    let _ = self.help.try_remove_front(orb);
                    return;
                }
                OperationState::PreCas => {
                    let cas_list = match self.algorithm.generator(&or.input, &mut ContentionMeasure(0)) {
                        Ok(cas_list) => cas_list,
                        Err(Contention) => {
                            continue;
                        }
                    };
                    Box::new(OperationRecord {
                        owner: or.owner.clone(),
                        state: OperationState::ExecuteCas(cas_list),
                        input: or.input.clone(),
                    })
                }
                OperationState::ExecuteCas(cas_list) => {
                    let outcome = self.cas_executor(cas_list, &mut ContentionMeasure(0));
                    Box::new(OperationRecord {
                        owner: or.owner.clone(),
                        state: OperationState::PostCas(cas_list.clone(), outcome),
                        input: or.input.clone(),
                    })
                }
                OperationState::PostCas(cas_list, outcome) => {
                    match self.algorithm.wrap_up(*outcome, cas_list, &mut ContentionMeasure(0)) {
                        Ok(Some(result)) => {
                            Box::new(OperationRecord {
                                owner: or.owner.clone(),
                                state: OperationState::Completed(result),
                                input: or.input.clone(),
                            })
                        }
                        Ok(None) => {
                            Box::new(OperationRecord {
                                owner: or.owner.clone(),
                                state: OperationState::PreCas,
                                input: or.input.clone(),
                            })
                        }
                        Err(_) => {
                            continue;
                        }
                    }
                }
            };

            let updated_or = Box::into_raw(updated_or);
            if orb.val.compare_exchange_weak(or as *const OperationRecord<_> as *mut OperationRecord<_>, updated_or, Ordering::SeqCst, Ordering::SeqCst).is_err() {
                // Never got shared, so safe to drop
                let _ = unsafe { Box::from_raw(updated_or) };
            }
        }
    }

    pub fn run(&self, op: LF::Input) -> LF::Output {
        for retry in 0.. {
            let help = /* one in a while */ true;
            if help {
                self.help_first();
            }
            let mut contention = ContentionMeasure(0);
            let cases = self.algorithm.generator(&op, &mut contention);
            if contention.use_slow_path() {
                break;
            }
            let result = self.cas_executor(&cases, &mut contention);
            if contention.use_slow_path() {
                break;
            }
            match self.algorithm.wrap_up(result, &cases, &mut contention) {
                Ok(outcome) => return outcome,
                Err(_) => {}
            }
            if contention.use_slow_path() {
                break;
            }
            if retry > RETRY_THRESHOLD {
                break;
            }
        }

        let orb = OperationRecordBox {
            val: AtomicPtr::new(Box::into_raw(Box::new(OperationRecord {
                owner: std::thread::current().id(),
                input: op,
                state: OperationState::PreCas,
            })))
        };

        self.help.enqueue(&orb);
        // Safety: ???

        loop {
            let or = unsafe { &*orb.val.load(Ordering::SeqCst) };
            if let OperationState::Completed(t) = &or.state {
                break t.clone();
            } else {
                self.help_first()
            }
        }

        // Extract value from completed and return
    }
}

// in a consuming crate (wait-free-linked-list crate)
// pub struct WaitFreeLinkedList<T> {
//     simulator: WaitFreeSimulator<LockFreeLinkedList<T>>,
// }
//
// struct LockFreeLinkedList<T> {
//     t: T,
// }

// impl<T> NormalizedLockFree for LockFreeLinkedList<T> {}
//
// impl<T> WaitFreeLinkedList<T> {
//     pub fn push_front(&mut self, t: T) {
//         let i = self.simulator.run(Insert(t));
//         self.simulator.wait_for(i)
//     }
// }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {}
}
