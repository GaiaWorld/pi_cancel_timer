//! 基于多层时间轮与溢出堆的可取消定时器。
//!
//! 所有时间参数均为调用方同一时钟原点下的 `u64` 毫秒值，本库不读取系统时钟。
//! 使用 `Timer::new` 指定起点，随后由 `pop` 按传入的时间推进内部刻度。
//! 近期任务放入时间轮，远期任务在堆中按绝对到期时间排序；迁入轮时取消键保持不变。
//!
//! # 示例
//!
//! ```rust
//! use pi_cancel_timer::Timer;
//!
//! let mut timer: Timer<&str, 4, 2, 1, 10> = Timer::new(101);
//! let key = timer.schedule_at(116, "任务");
//! assert_eq!(timer.count(), 1);
//! assert!(!timer.has_task_by(120));
//! assert_eq!(timer.pop(121), Some("任务"));
//! assert_eq!(timer.cancel(key), None); // 弹出后取消键失效
//! ```

use std::{cmp::Reverse, collections::BinaryHeap, fmt};

use pi_slot_deque::Deque;
use pi_slot_wheel::{Result as WheelResult, Slot, TimeoutItem, TimerKey, Wheel};

/// 可取消的定时器。
///
/// `N0` 为首层槽数，`N` 为其余各层槽数，`L` 为其余层数；
/// `PRECISION_MS` 为每个刻度的毫秒数，默认 1，构造时拒绝零精度。
/// 时间轮的完整周期为 `N0 * N^L` 个刻度；超出轮范围的任务由堆管理。
/// 当前有效任务数由共享存储的长度决定，不包含已取消的堆索引。
pub struct Timer<T, const N0: usize, const N: usize, const L: usize, const PRECISION_MS: u64 = 1> {
    /// 轮内队列与外部队列共用的存储，保存所有未弹出、未取消的任务。
    slot: Slot<TimerKey, TimeoutItem<T>>,
    /// 管理近期任务；每次滚动推进一个 PRECISION_MS 刻度。
    wheel: Wheel<T, N0, N, L>,
    /// 持有所有有效的轮外任务，提供按稳定键删除及迁入轮的来源队列。
    external: Deque<TimerKey, TimeoutItem<T>>,
    /// 按向上对齐后的绝对到期毫秒排序；取消后索引留在堆内等待清理。
    heap: BinaryHeap<Reverse<(u64, TimerKey)>>,
    /// 第零刻度对应的外部绝对毫秒值，不随轮滚动而改变。
    origin_ms: u64,
    /// 从第零刻度开始累计的推进次数；与轮当前周期内的位置不同。
    elapsed_ticks: u64,
}

impl<T: fmt::Debug, const N0: usize, const N: usize, const L: usize, const PRECISION_MS: u64>
    fmt::Debug for Timer<T, N0, N, L, PRECISION_MS>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Timer")
            .field("slot_len", &self.slot.len())
            .field("wheel", &self.wheel)
            .field("external", &self.external)
            .field("heap", &self.heap)
            .field("origin_ms", &self.origin_ms)
            .field("elapsed_ticks", &self.elapsed_ticks)
            .finish()
    }
}

impl<T, const N0: usize, const N: usize, const L: usize, const PRECISION_MS: u64> Default
    for Timer<T, N0, N, L, PRECISION_MS>
{
    /// 以调用方时钟的零点为起点；非零起点使用 `new(start_ms)`。
    fn default() -> Self {
        Self::new(0)
    }
}

impl<T, const N0: usize, const N: usize, const L: usize, const PRECISION_MS: u64>
    Timer<T, N0, N, L, PRECISION_MS>
{
    /// 在调用方的时钟 `start_ms` 处创建定时器，不读取真实时间。
    /// 后续所有截止时间及查询时间须与它使用相同的时钟原点和毫秒单位。
    /// `PRECISION_MS == 0` 或轮维度无效、容量溢出时会 panic。
    pub fn new(start_ms: u64) -> Self {
        assert!(PRECISION_MS > 0, "timer precision must be positive");
        Self {
            slot: Slot::default(),
            wheel: Wheel::default(),
            external: Deque::new(),
            heap: BinaryHeap::new(),
            origin_ms: start_ms,
            elapsed_ticks: 0,
        }
    }

    /// 返回内部所在刻度的绝对毫秒值。
    /// 若调用方传入的时间未落在刻度边界，本值会向下取整，可能小于传入值。
    pub fn current_time_ms(&self) -> u64 {
        self.origin_ms + self.elapsed_ticks * PRECISION_MS
    }

    /// 返回当前仍在定时器中的任务总数，包括轮内和轮外的远期任务。
    /// 已弹出或已取消的任务不计入，堆里尚未清理的失效索引也不计入。
    pub fn count(&self) -> usize {
        self.slot.len()
    }

    /// 在当前内部刻度后的 `delay_ms` 毫秒安排任务，返回可用于 `cancel` 的稳定键。
    /// 这里的“当前”指 `current_time_ms()`，不是调用方的实时钟值；
    /// 小于一个刻度的非零延迟会向上对齐到下一刻度。
    /// 相加或对齐后的绝对截止时间超出 `u64` 时 panic，任务不会被插入。
    pub fn schedule_after(&mut self, delay_ms: u64, value: T) -> TimerKey {
        let deadline_ms = self
            .current_time_ms()
            .checked_add(delay_ms)
            .expect("timer deadline overflow");
        self.schedule_at(deadline_ms, value)
    }

    /// 在绝对时间 `deadline_ms` 安排任务，返回可用于取消的稳定键。
    /// 指定过去时间或当前时间时放入当前刻度；将来时间向上对齐到刻度，
    /// 因此任务不会早于指定时间弹出。无论存放在轮还是堆中，键都保持有效。
    /// 对齐后的绝对截止时间超出 `u64` 时 panic，任务不会被插入。
    pub fn schedule_at(&mut self, deadline_ms: u64, value: T) -> TimerKey {
        let remaining = deadline_ms.saturating_sub(self.current_time_ms());
        let ticks = remaining / PRECISION_MS + u64::from(remaining % PRECISION_MS != 0);
        let aligned_deadline = self
            .current_time_ms()
            .checked_add(
                ticks
                    .checked_mul(PRECISION_MS)
                    .expect("timer deadline overflow"),
            )
            .expect("timer deadline overflow");
        if ticks < self.wheel.max_time() as u64 {
            match self.wheel.push(ticks as usize, value, &mut self.slot) {
                WheelResult::Ok(key) => key,
                WheelResult::Overflow(_, _) => unreachable!("wheel rejected an in-range delay"),
            }
        } else {
            let key = self
                .external
                .ops(&mut self.slot)
                .push_back(TimeoutItem::new(
                    0,
                    value,
                    Wheel::<T, N0, N, L>::external_index(),
                ));
            self.heap.push(Reverse((aligned_deadline, key)));
            key
        }
    }

    /// 只读判断截至 `now_ms` 是否可能有尚未处理的任务。
    /// 同时检查时间轮和有效的堆任务；`false` 表示肯定没有，
    /// `true` 可能来自轮的粗粒度高层槽，不保证此时调用 `pop` 必有结果。
    /// 本方法不推进内部时钟，也不清理失效堆条目；若内部当前刻度已有任务，
    /// 即使 `now_ms` 早于内部时间，也可能返回 `true`。
    pub fn has_task_by(&self, now_ms: u64) -> bool {
        let ticks = now_ms.saturating_sub(self.current_time_ms()) / PRECISION_MS;
        if self
            .wheel
            .has_task_by(ticks.min(usize::MAX as u64) as usize)
        {
            return true;
        }
        match self.heap.peek() {
            Some(&Reverse((deadline, key))) if self.slot.contains_key(key) => deadline <= now_ms,
            Some(_) => self.heap.iter().any(|Reverse((deadline, key))| {
                *deadline <= now_ms && self.slot.contains_key(*key)
            }),
            None => false,
        }
    }

    /// 弹出一项截至 `now_ms` 可处理的任务。
    /// 先弹内部当前刻度的任务；当前槽为空时才逐刻度推进，直到达到不超过
    /// `now_ms` 的最近刻度。重复调用可逐项取完，返回 `None` 时说明
    /// 可到达的刻度中没有待处理任务，内部时间已推进到目标刻度。
    /// 传入过去时间不会倒退内部时钟；此前插入当前刻度的逾期任务仍可弹出。
    pub fn pop(&mut self, now_ms: u64) -> Option<T> {
        loop {
            if let Some(item) = self.wheel.pop(&mut self.slot) {
                return Some(item.el);
            }
            if self
                .current_time_ms()
                .checked_add(PRECISION_MS)
                .is_none_or(|next| next > now_ms)
            {
                return None;
            }
            self.roll();
        }
    }

    /// 用安排任务时返回的稳定键取消任务，并立即取回其值。
    /// 无效键、已经取消或已经弹出的键返回 `None`。轮内任务直接移除；
    /// 轮外任务从外部队列移除，堆中的索引暂时保留，不再计入 `count()`。
    /// 失效索引至少 64 个且达到堆的一半时批量重建；否则在遇到堆顶时清理。
    pub fn cancel(&mut self, key: TimerKey) -> Option<T> {
        let item = if self.slot.get(key)?.index == Wheel::<T, N0, N, L>::external_index() {
            let item = self.external.ops(&mut self.slot).remove(key)?;
            // 堆中每个有效条目恰好对应一个外部队列任务，差值即惰性失效条目数。
            let stale_count = self.heap.len() - self.external.len();
            if stale_count >= 64 && stale_count >= self.heap.len() / 2 {
                self.heap
                    .retain(|Reverse((_, key))| self.slot.contains_key(*key));
            }
            item
        } else {
            self.wheel.remove(key, &mut self.slot)?
        };
        Some(item.el)
    }

    /// 将内部时间推进一个刻度；轮完成整圈时才检查堆顶。
    /// 进入轮覆盖范围的任务从外部队列迁入轮，保持原取消键；
    /// 已取消的堆顶索引直接清理，不扫描其他远期有效条目。
    fn roll(&mut self) {
        self.elapsed_ticks += 1;
        if self.wheel.roll(&mut self.slot) {
            while let Some(&Reverse((deadline, key))) = self.heap.peek() {
                if !self.slot.contains_key(key) {
                    self.heap.pop();
                    continue;
                }
                let remaining = deadline.saturating_sub(self.current_time_ms());
                let ticks = remaining / PRECISION_MS + u64::from(remaining % PRECISION_MS != 0);
                if ticks >= self.wheel.max_time() as u64 {
                    break;
                }
                self.heap.pop();
                assert!(self.wheel.move_from_external(
                    ticks as usize,
                    key,
                    &mut self.external,
                    &mut self.slot,
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Timer;

    #[test]
    fn absolute_deadlines_and_cancelled_keys_survive_wheel_cycles() {
        let mut timer: Timer<&str, 4, 2, 1> = Timer::new(1000);
        assert_eq!(timer.count(), 0);
        let first = timer.schedule_at(1017, "first");
        let cancelled = timer.schedule_at(1025, "cancelled");
        assert_eq!(timer.count(), 2);
        assert_eq!(timer.cancel(cancelled), Some("cancelled"));
        assert_eq!(timer.count(), 1);
        assert_eq!(timer.pop(1016), None);
        assert_eq!(timer.current_time_ms(), 1016);
        assert_eq!(timer.count(), 1);
        assert_eq!(timer.pop(1017), Some("first"));
        assert_eq!(timer.count(), 0);
        assert_eq!(timer.cancel(first), None);
        assert_eq!(timer.pop(1030), None);
    }

    #[test]
    fn precision_rounds_deadlines_up_and_queries_down() {
        let mut timer: Timer<&str, 4, 2, 1, 10> = Timer::new(101);
        let key = timer.schedule_at(116, "event");
        assert!(!timer.has_task_by(120));
        assert!(timer.has_task_by(121));
        assert_eq!(timer.pop(120), None);
        assert_eq!(timer.current_time_ms(), 111);
        assert_eq!(timer.pop(121), Some("event"));
        assert_eq!(timer.cancel(key), None);
    }

    #[test]
    fn overflow_queue_compacts_cancelled_entries_and_keeps_live_keys() {
        let mut timer: Timer<usize, 4, 2, 1> = Timer::new(500);
        let live = timer.schedule_after(1000, 999);
        assert_eq!(timer.count(), 1);
        for i in 0..200 {
            let key = timer.schedule_after(10000 + i, i as usize);
            assert_eq!(timer.count(), 2);
            assert_eq!(timer.cancel(key), Some(i as usize));
            assert_eq!(timer.count(), 1);
            assert_eq!(
                timer.heap.len() - timer.external.len(),
                ((i + 1) % 64) as usize
            );
        }
        assert!(timer.heap.len() < 64);
        assert_eq!(timer.heap.len() - timer.external.len(), 200 % 64);
        assert!(timer.has_task_by(1500));
        assert_eq!(timer.pop(1499), None);
        assert_eq!(timer.cancel(live), Some(999));
        assert_eq!(timer.count(), 0);
        assert!(!timer.has_task_by(1500));
    }

    #[test]
    fn overflow_migration_retains_cancellation_key() {
        let mut timer: Timer<&str, 4, 2, 1> = Timer::new(100);
        let key = timer.schedule_at(117, "moved");
        assert_eq!(timer.pop(116), None);
        assert_eq!(timer.cancel(key), Some("moved"));
        assert_eq!(timer.pop(117), None);
    }

    #[test]
    fn cancelled_heap_head_does_not_hide_later_due_task() {
        let mut timer: Timer<&str, 4, 2, 1> = Timer::new(100);
        let cancelled = timer.schedule_at(120, "cancelled");
        timer.schedule_at(125, "live");
        assert_eq!(timer.cancel(cancelled), Some("cancelled"));
        assert!(!timer.has_task_by(124));
        assert!(timer.has_task_by(125));
        assert_eq!(timer.pop(125), Some("live"));
        assert_eq!(timer.heap.len(), timer.external.len());
    }

    #[test]
    fn stale_count_remains_derivable_after_migration_and_cleanup() {
        let mut timer: Timer<&str, 4, 2, 1> = Timer::new(100);
        let cancelled = timer.schedule_at(109, "cancelled");
        let live = timer.schedule_at(125, "live");
        assert_eq!(timer.cancel(cancelled), Some("cancelled"));
        assert_eq!(timer.heap.len() - timer.external.len(), 1);
        assert_eq!(timer.pop(108), None);
        assert_eq!(timer.heap.len() - timer.external.len(), 0);
        assert_eq!(timer.cancel(live), Some("live"));
        assert_eq!(timer.heap.len() - timer.external.len(), 1);
        assert_eq!(timer.pop(124), None);
        assert_eq!(timer.heap.len(), timer.external.len());
    }

    #[test]
    fn precision_applies_to_overflow_tasks_after_migration() {
        let mut timer: Timer<&str, 4, 2, 1, 10> = Timer::new(101);
        let key = timer.schedule_at(267, "late");
        assert!(!timer.has_task_by(270));
        assert!(timer.has_task_by(271));
        assert_eq!(timer.pop(270), None);
        assert_eq!(timer.pop(271), Some("late"));
        assert_eq!(timer.cancel(key), None);
    }

    #[test]
    fn pending_current_task_remains_available_for_older_now() {
        let mut timer: Timer<&str, 4, 2, 1> = Timer::new(100);
        assert_eq!(timer.pop(105), None);
        timer.schedule_at(90, "overdue");
        assert!(timer.has_task_by(90));
        assert_eq!(timer.pop(90), Some("overdue"));
        assert_eq!(timer.current_time_ms(), 105);
    }

    #[test]
    #[should_panic(expected = "timer deadline overflow")]
    fn unrepresentable_quantized_deadline_is_rejected() {
        let mut timer: Timer<(), 4, 2, 1, 10> = Timer::new(u64::MAX - 5);
        timer.schedule_at(u64::MAX, ());
    }
}
