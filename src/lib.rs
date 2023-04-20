use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicPtr, Ordering};

use crate::help_queue::HelpQueue;

mod help_queue;

const CONTENTION_THRESHOLD: usize = 2;
const RETRY_THRESHOLD: usize = 2;

#[derive(Debug)]
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
#[derive(Eq, PartialEq)]
pub enum CasState {
    Success,
    Failure,
    Pending,
}

pub struct CasByRcu<T> {
    version: u64,
    value: T,
}

pub struct Atomic<T>(AtomicPtr<CasByRcu<T>>);

pub trait VersionedCas {
    fn execute(&self, contention: &mut ContentionMeasure) -> Result<bool, Contention>;
    fn has_modified_bit(&self) -> bool;
    fn clear_bit(&self) -> bool;
    fn state(&self) -> CasState;
    fn set_state(&self, new: CasState);
}

impl<T> Atomic<T>
    where
        T: Eq + PartialEq
{
    pub fn new(initial: T) -> Self {
        Self(AtomicPtr::new(Box::into_raw(Box::new(CasByRcu {
            version: 0,
            value: initial,
        }))))
    }

    pub fn get2(&self) -> (&T, u64) {
        let this_ptr = self.get();
        let this = unsafe { &*this_ptr };
        (&this.value, this.version)
    }
    pub fn with<F, R>(&self, f: F) -> R where F: FnOnce(&T, u64) -> R {
        let this_ptr = self.get();
        let this = unsafe { &*this_ptr };
        f(&this.value, this.version)
    }

    fn get(&self) -> *mut CasByRcu<T> {
        self.0.load(Ordering::SeqCst)
    }

    fn _set(&self, value: T) {
        let this_ptr = self.get() as *mut CasByRcu<T>;
        let this = unsafe { &*this_ptr };
        if this.value != value {
            let _ = self.0.store(
                Box::into_raw(
                    Box::new(CasByRcu {
                        version: this.version + 1,
                        value,
                    })),
                Ordering::SeqCst);
        }
    }

    pub fn value(&self) -> &T {
        // Safety: this is safe because we never deallocate
        &unsafe { &*self.0.load(Ordering::SeqCst) }.value
    }

    pub fn compare_and_set(&self, expected: &T, value: T, contention: &mut ContentionMeasure, version: Option<u64>) -> Result<bool, Contention> {
        let this_ptr = self.get();
        let this = unsafe { &*this_ptr };
        if &this.value == expected {
            if let Some(v) = version {
                if v != this.version {
                    contention.detected()?;
                    return Ok(false);
                }
            }
            if expected != &value {
                let new = Box::into_raw(
                    Box::new(CasByRcu {
                        version: this.version + 1,
                        value,
                    }));
                return match self.0.compare_exchange(this_ptr,
                                                     new,
                                                     Ordering::SeqCst, Ordering::Relaxed) {
                    Ok(_) => Ok(true),
                    Err(_) => {
                        // Safety: the Box was never shared
                        let _ = unsafe { Box::from_raw(new) };
                        contention.detected()?;
                        Ok(false)
                    }
                };
            }
            return Ok(true);
        }
        Ok(false)
    }

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

    fn set_state(&self, _new: &CasState) {
        todo!()
    }
}


// in bystander
pub trait NormalizedLockFree {
    type CommitDescriptor: Clone;
    type Output: Clone;
    type Input: Clone;


    fn generator(&self, op: &Self::Input, contention: &mut ContentionMeasure) -> Result<Self::CommitDescriptor, Contention>;
    fn wrap_up(&self, executed: Result<(), usize>, performed: &Self::CommitDescriptor, contention: &mut ContentionMeasure) -> Result<Option<Self::Output>, Contention>;
    fn fast_path(&self, op: &Self::Input, contention: &mut ContentionMeasure) -> Result<Self::Output, Contention>;
}

pub struct OperationRecordBox<LF: NormalizedLockFree> {
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

struct Shared<LF: NormalizedLockFree, const N: usize> {
    algorithm: LF,
    help: HelpQueue<LF, N>,
    free_ids: Mutex<Vec<usize>>,
}

pub struct TooManyHandles;

pub struct WaitFreeSimulator<LF: NormalizedLockFree, const N: usize> {
    shared: Arc<Shared<LF, N>>,

    id: usize,
}

impl<LF: NormalizedLockFree, const N: usize> WaitFreeSimulator<LF, N> {
    pub fn new(algorithm: LF) -> Self {
        assert_ne!(N, 0);
        Self {
            shared: Arc::new(Shared {
                algorithm,
                help: HelpQueue::new(),
                free_ids: Mutex::new((1..N).collect())
            }),
            id: 0
        }
    }

    pub fn fork(&self) -> Result<Self, TooManyHandles> {
        if let Some(id) = self.shared.free_ids.lock().unwrap().pop() {
            Ok(Self {
                shared: Arc::clone(&self.shared),
                id,
            })
        } else {
            Err(TooManyHandles)
        }
    }
}

impl<LF: NormalizedLockFree, const N: usize> Drop for WaitFreeSimulator<LF, N> {
    fn drop(&mut self) {
        self.shared.free_ids.lock().unwrap().push(self.id)
    }
}


pub enum CasExecutionFailure {
    CasFailed(usize),
    Contention,
}

impl From<Contention> for CasExecutionFailure {
    fn from(_value: Contention) -> Self {
        Self::Contention
    }
}


impl<LF: NormalizedLockFree, const N: usize> WaitFreeSimulator<LF, N>
    where
            for<'a> &'a LF::CommitDescriptor: IntoIterator<Item=&'a dyn VersionedCas>
{
    pub fn cas_executor(&self, descriptors: &LF::CommitDescriptor, contention: &mut ContentionMeasure) -> Result<(), CasExecutionFailure> {
        for (i, cas) in descriptors.into_iter().enumerate() {
            match cas.state() {
                CasState::Success => {
                    cas.clear_bit();
                }
                CasState::Failure => {
                    return Err(CasExecutionFailure::CasFailed(i));
                }
                CasState::Pending => {
                    cas.execute(contention)?;
                    if cas.has_modified_bit() {
                        cas.set_state(CasState::Success);
                        cas.clear_bit();
                    }
                    if cas.state() != CasState::Success {
                        cas.set_state(CasState::Failure);
                        return Err(CasExecutionFailure::CasFailed(i));
                    }
                }
            }
        }
        Ok(())
    }

    fn help_first(&self) {
        if let Some(help) = self.shared.help.peek() {
            self.help_op(unsafe { &*help })
        }
    }

    // Guarantees that on return, orb is no longer in help queue
    fn help_op(&self, orb: &OperationRecordBox<LF>) {
        loop {
            let or = unsafe { &*orb.val.load(Ordering::SeqCst) };
            let updated_or = match &or.state {
                OperationState::Completed(..) => {
                    let _ = self.shared.help.try_remove_front(orb);
                    return;
                }
                OperationState::PreCas => {
                    let cas_list = match self.shared.algorithm.generator(&or.input, &mut ContentionMeasure(0)) {
                        Ok(cas_list) => cas_list,
                        Err(_) => {
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
                    let outcome = match self.cas_executor(cas_list, &mut ContentionMeasure(0)) {
                        Ok(outcome) => Ok(outcome),
                        Err(CasExecutionFailure::CasFailed(i)) => Err(i),
                        Err(_) => continue
                    };
                    Box::new(OperationRecord {
                        owner: or.owner.clone(),
                        state: OperationState::PostCas(cas_list.clone(), outcome),
                        input: or.input.clone(),
                    })
                }
                OperationState::PostCas(cas_list, outcome) => {
                    match self.shared.algorithm.wrap_up(*outcome, cas_list, &mut ContentionMeasure(0)) {
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
        let help = /* one in a while */ true;
        if help {
            self.help_first();
        }

        // slow path: ask for help
        for retry in 0.. {
            let mut contention = ContentionMeasure(0);

            match self.shared.algorithm.fast_path(&op, &mut contention) {
                Ok(result) => return result,
                Err(_) => {}
            }

            // let cases = match self.algorithm.generator(&op, &mut contention) {
            //     Ok(cases) => cases,
            //     Err(_) => break
            // };
            //
            // let result = match self.cas_executor(&cases, &mut contention) {
            //   Ok(result) => result,
            //     Err(_) => continue
            // };
            // match self.algorithm.wrap_up(result, &cases, &mut contention) {
            //     Ok(Some(outcome)) => return outcome,
            //     Ok(None) => {}
            //     Err(_) => break
            // }
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

        self.shared.help.enqueue(self.id, &orb);
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



