//! Process-lifetime quarantine when a native callback/resource cannot retire.
use std::{
    mem::ManuallyDrop,
    ops::{Deref, DerefMut},
};
pub(super) struct NativeRetention<T> {
    value: ManuallyDrop<T>,
    settled: bool,
}
impl<T> NativeRetention<T> {
    pub(super) fn new(value: T) -> Self {
        Self {
            value: ManuallyDrop::new(value),
            settled: false,
        }
    }
    /// Caller has native terminal-callback, original instance termination and
    /// endpoint cleanup receipts. A deadline/cancel/Drop never calls this.
    pub(super) fn mark_settled(&mut self) {
        self.settled = true;
    }
}
impl<T> Deref for NativeRetention<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}
impl<T> DerefMut for NativeRetention<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.value
    }
}
impl<T> Drop for NativeRetention<T> {
    fn drop(&mut self) {
        if self.settled {
            unsafe { ManuallyDrop::drop(&mut self.value) };
        }
    }
}
