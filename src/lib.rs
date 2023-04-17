use std::marker::PhantomData;
use std::ops::Index;
use std::slice::SliceIndex;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};

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

pub trait CasDescriptor {
    fn execute(&self) -> Result<(), ()>;
}

pub trait CasDescriptors<D>: Index<usize, Output=D>
    where
        D: CasDescriptor
{
    fn len(&self) -> usize;
}


// in bystander
pub trait NormalizedLockFree {
    type Cas: CasDescriptor;
    type Cases: CasDescriptors<Self::Cas> + Clone;
    type Output: Clone;
    type Input: Clone;


    fn generator(&self, op: &Self::Input, contention: &mut ContentionMeasure) -> Result<Self::Cases, Contention>;
    fn wrap_up(&self, executed: Result<(), usize>, performed: &Self::Cases, contention: &mut ContentionMeasure) -> Result<Option<Self::Output>, Contention>;
}

struct OperationRecordBox<LF: NormalizedLockFree> {
    val: AtomicPtr<OperationRecord<LF>>,
}

enum OperationState<LF: NormalizedLockFree> {
    PreCas,
    ExecuteCas(LF::Cases),
    PostCas(LF::Cases, Result<(), usize>),
    Completed(LF::Output),
}

// impl<LF: NormalizedLockFree> OperationState<LF> {
//     pub fn is_completed(&self) -> bool {
//         matches!(self, Self::Completed(..))
//     }
// }

struct OperationRecord<LF: NormalizedLockFree> {
    owner: std::thread::ThreadId,
    state: OperationState<LF>,
    input: LF::Input,
}

// impl<LF: NormalizedLockFree> Clone for OperationRecord<LF>
//     where
//         LF::Input: Clone,
//         LF::Output: Clone,
//         LF::Cases: Clone
// {
//     fn clone(&self) -> Self {
//         Self {
//             owner: self.owner.clone(),
//             input: self.input.clone(),
//             state: self.state.clone(),
//             cas_list: self.cas_list.clone(),
//         }
//     }
// }

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
{
    fn help_first(&self) {
        if let Some(help) = self.help.peek() {
            self.help_op(unsafe { &*help })
        }
    }

    fn cas_executor(&self, descriptors: &LF::Cases, content: &mut ContentionMeasure) -> Result<(), usize> {
        let len = descriptors.len();
        for i in 0..len {
            // TODO: Check if already complete
            if descriptors[i].execute().is_err() {
                content.detected();
                return Err(i);
            }
        }
        Ok(())
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
