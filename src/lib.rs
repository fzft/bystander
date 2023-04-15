use std::sync::atomic::{AtomicBool, Ordering};

pub struct ContentionMeasure(usize);

impl ContentionMeasure {
    pub fn detected(&mut self) {
        self.0 += 1
    }
}


// in bystander
pub trait NormalizedLockFree {
    type CasDescriptor;
    type Output;
    type Input;

    fn prepare(&self, op: &Self::Input) -> [Self::CasDescriptor];
    fn execute(&self, cases: &[Self::CasDescriptor], contention: &mut ContentionMeasure) -> Result<(), usize>;
    fn cleanup(&self);
}

pub struct Help {
    completed: AtomicBool
}

// A wait-free queue
pub struct HelpQueue;

impl HelpQueue {
    pub fn add(&self, help: *const Help) {}

    pub fn peek(&self) -> Option<*const Help> {}

    pub fn try_remove_front(&self, &Help) {}
}

pub struct WaitFreeSimulator<LF: NormalizedLockFree> {
    algorithm: LF,

}


impl<LF: NormalizedLockFree> WaitFreeSimulator<LF> {
    fn help(&self) {
        if let Some(help) = self.help.peek() {}
    }


    pub fn run(&self, op: LF::Input) -> LF::Output {
        if false {
            self.help()
        }

        let mut contention = ContentionMeasure(0);
        let cas = self.algorithm.prepare(&op);
        match self.algorithm.execute(&cas[..], &mut contention) {
            Ok(_) => {
                self.algorithm.cleanup();
            }
            Err(i) => {
                let help = Help {
                    completed: AtomicBool::new(false)
                };
                self.help.add(&help);
                while !help.completed.load(Ordering::SeqCst) {
                    self.help();
                }
            }
        }
    }
}

// in a consuming crate (wait-free-linked-list crate)
pub struct WaitFreeLinkedList<T> {
    simulator: WaitFreeSimulator<LockFreeLinkedList<T>>,
}

struct LockFreeLinkedList<T> {
    t: T,
}

impl<T> NormalizedLockFree for LockFreeLinkedList<T> {}

impl<T> WaitFreeLinkedList<T> {
    pub fn push_front(&mut self, t: T) {
        let i = self.simulator.run(Insert(t));
        self.simulator.wait_for(i)
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {}
}
