//! Lock helpers that recover from poisoning instead of cascading a panic.
//!
//! A mutex is poisoned when a thread panics while holding its guard. The
//! standard library then fails every later `lock()`, and the usual
//! `lock().unwrap()` turns one panic (say, in a background task) into a
//! panic in every thread that touches the same state, including the UI.
//!
//! Recovering with [`PoisonError::into_inner`] is safe for the state guarded
//! in Kubach: caches, registries, subscriber lists, the current client of a
//! session, audit buffers. Each critical section is a single assignment,
//! push, insert, take or clone, so a panic cannot leave the data half
//! updated; at worst an entry is missing, which the next refresh or watch
//! event repairs. Where a critical section does keep a multi-step
//! invariant, lock it directly and let the poison propagate, with a comment
//! saying why.

use std::sync::{Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// Locks `mutex`, taking the guard of a poisoned one.
pub fn lock<T: ?Sized>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Read-locks `lock`, taking the guard of a poisoned one.
pub fn read<T: ?Sized>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(PoisonError::into_inner)
}

/// Write-locks `lock`, taking the guard of a poisoned one.
pub fn write<T: ?Sized>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(PoisonError::into_inner)
}

/// Consumes `mutex` and returns its value, poisoned or not.
pub fn into_inner<T>(mutex: Mutex<T>) -> T {
    mutex.into_inner().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, RwLock};
    use std::thread;

    use super::*;

    /// Panics on another thread while holding the guard.
    fn poison_mutex(mutex: &Arc<Mutex<Vec<u32>>>) {
        let held = Arc::clone(mutex);
        let result = thread::spawn(move || {
            let mut guard = held.lock().unwrap();
            guard.push(2);
            panic!("poison the mutex");
        })
        .join();
        assert!(result.is_err());
        assert!(mutex.is_poisoned());
    }

    #[test]
    fn lock_recovers_a_poisoned_mutex() {
        let mutex = Arc::new(Mutex::new(vec![1]));
        poison_mutex(&mutex);

        // What the panicking thread wrote before panicking is kept.
        assert_eq!(*lock(&mutex), vec![1, 2]);
        lock(&mutex).push(3);
        assert_eq!(*lock(&mutex), vec![1, 2, 3]);
    }

    #[test]
    fn into_inner_recovers_a_poisoned_mutex() {
        let mutex = Arc::new(Mutex::new(vec![1]));
        poison_mutex(&mutex);
        let mutex = Arc::into_inner(mutex).unwrap();
        assert_eq!(into_inner(mutex), vec![1, 2]);
    }

    #[test]
    fn read_and_write_recover_a_poisoned_rwlock() {
        let rw = Arc::new(RwLock::new(1));
        let held = Arc::clone(&rw);
        let result = thread::spawn(move || {
            let mut guard = held.write().unwrap();
            *guard = 2;
            panic!("poison the lock");
        })
        .join();
        assert!(result.is_err());
        assert!(rw.is_poisoned());

        assert_eq!(*read(&rw), 2);
        *write(&rw) = 3;
        assert_eq!(*read(&rw), 3);
    }

    #[test]
    fn lock_works_on_a_healthy_mutex() {
        let mutex = Mutex::new(5);
        *lock(&mutex) += 1;
        assert_eq!(*lock(&mutex), 6);
    }
}
