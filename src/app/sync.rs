//! 锁辅助（共享）：锁中毒恢复。
//!
//! 由 banqi-gui/src/state.rs（`lock`）收敛而来；server 侧各处
//! `.expect("… 锁中毒")` 同步替换，统一语义：任一线程 panic 污染锁后
//! 显式恢复内部数据继续，避免整个后端连锁不可用。

use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// 获取互斥锁；若此前有线程 panic 导致锁中毒，恢复内部数据继续。
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// 获取读写锁读端；锁中毒时恢复内部数据继续。
pub fn read_lock<T>(rw: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    rw.read().unwrap_or_else(|e| e.into_inner())
}

/// 获取读写锁写端；锁中毒时恢复内部数据继续。
pub fn write_lock<T>(rw: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    rw.write().unwrap_or_else(|e| e.into_inner())
}
