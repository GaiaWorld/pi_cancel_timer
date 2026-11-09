//! 基于常量泛型的多层定时轮。
//! N0、N、L 分别是首层槽数、其余各层槽数和其余层数。
//! 所有队列共享调用方传入的同一份 Slot；任务跨层迁移时键保持有效。

use pi_key::new_key_type;
use pi_null::Null;
use pi_slot_deque::Deque;
pub use pi_slot_deque::DequeStorage as Slot;
use std::{cmp::Ordering, fmt};

// 定义定时任务的稳定键类型。
new_key_type! {
    pub struct TimerKey;
}

/// 插入任务的结果。
pub enum Result<T> {
    /// 插入成功，返回稳定键。
    Ok(TimerKey),
    /// 未插入，返回修正后的时间和原任务；修正时间溢出时饱和为 usize::MAX。
    Overflow(usize, T),
}
/// 多层定时轮。三个维度都必须大于零，且 N0 * N^L 不得超过 usize。
/// 创建时校验维度，不满足时 panic；所有操作必须使用同一份 Slot。
pub struct Wheel<T, const N0: usize, const N: usize, const L: usize> {
    /// 首层轮
    layer0: [Deque<TimerKey, TimeoutItem<T>>; N0],
    /// 多层定时轮
    layers: [[Deque<TimerKey, TimeoutItem<T>>; N]; L],
    /// 首层轮的当前滚动到的位置
    index: usize,
    /// 每层的当前滚动到的位置
    indexs: [usize; L],
}
impl<T, const N0: usize, const N: usize, const L: usize> Default for Wheel<T, N0, N, L> {
    fn default() -> Self {
        Self::validate_dimensions();
        Wheel {
            layer0: std::array::from_fn(|_| Deque::new()),
            layers: std::array::from_fn(|_| std::array::from_fn(|_| Deque::new())),
            index: 0,
            indexs: [0; L],
        }
    }
}

impl<T: fmt::Debug, const N0: usize, const N: usize, const L: usize> fmt::Debug
    for Wheel<T, N0, N, L>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Wheel")
            .field("layer0", &self.layer0)
            .field("layers", &self.layers)
            .field("index", &self.index)
            .field("indexs", &self.indexs)
            .finish()
    }
}

impl<T, const N0: usize, const N: usize, const L: usize> Wheel<T, N0, N, L> {
    fn validate_dimensions() {
        assert!(
            N0 > 0 && N > 0 && L > 0,
            "wheel dimensions must be positive"
        );
        let mut capacity = N0;
        for _ in 0..L {
            capacity = capacity.checked_mul(N).expect("wheel capacity overflow");
        }
        N0.checked_add(N.checked_mul(L).expect("wheel index overflow"))
            .expect("wheel index overflow");
    }
    /// 返回轮内所有槽位之后的第一个索引，供外部队列的任务标记使用。
    pub const fn external_index() -> usize {
        N0 + N * L
    }
    /// 返回轮指针在当前完整周期内的位置；周期结束后从零开始。
    pub fn roll_count(&self) -> usize {
        let mut c = self.index;
        for i in 0..L {
            c += self.indexs[i] * (N0 * N.pow(i as u32));
        }
        c
    }
    /// 判断当前首层槽位是否为空；为空时返回 true。
    pub fn is_cur_over(&self) -> bool {
        self.layer0[self.index].head().is_null()
    }
    /// 在当前时刻之后 timeout 个刻度安排任务；成功后返回稳定键。
    /// 超出可容纳范围时返回 Overflow，原任务不会插入 Slot。
    pub fn push(
        &mut self,
        mut timeout: usize,
        el: T,
        slot: &mut Slot<TimerKey, TimeoutItem<T>>,
    ) -> Result<T> {
        if timeout < N0 {
            let j = (self.index + timeout) % N0;
            return Result::Ok(
                self.layer0[j]
                    .ops(slot)
                    .push_back(TimeoutItem::new(timeout, el, j)),
            );
        }
        let mut fix = self.index;
        for i in 0..L {
            let t = N0 * N.pow(i as u32);
            if timeout < t * N {
                timeout = (timeout + fix + self.indexs[i] * t) % (t * N);
                let j = timeout / t;
                return Result::Ok(self.layers[i][j].ops(slot).push_back(TimeoutItem::new(
                    timeout,
                    el,
                    N0 + i * N + j,
                )));
            }
            fix += self.indexs[i] * t;
        }
        Result::Overflow(timeout.saturating_add(fix), el)
    }
    /// 将外部队列的任务移入轮内，保留原键。timeout 是相对当前刻度的延迟。
    /// 超过轮容量或键不存在、并非外部任务时返回 false，任务不移动。
    /// 有效键必须属于传入的外部队列；队列与轮必须使用同一份 Slot。
    pub fn move_from_external(
        &mut self,
        timeout: usize,
        key: TimerKey,
        external: &mut Deque<TimerKey, TimeoutItem<T>>,
        slot: &mut Slot<TimerKey, TimeoutItem<T>>,
    ) -> bool {
        if timeout >= self.max_time()
            || slot
                .get(key)
                .is_none_or(|item| item.index != Self::external_index())
        {
            return false;
        }
        if timeout < N0 {
            let j = (self.index + timeout) % N0;
            let item = slot.get_mut(key).expect("external key missing");
            item.timeout = timeout;
            item.index = j;
            external.ops(slot).move_back_to(&mut self.layer0[j]);
            return true;
        }
        let mut fix = self.index;
        for i in 0..L {
            let t = N0 * N.pow(i as u32);
            if timeout < t * N {
                let corrected = (timeout + fix + self.indexs[i] * t) % (t * N);
                let j = corrected / t;
                let item = slot.get_mut(key).expect("external key missing");
                item.timeout = corrected;
                item.index = N0 + i * N + j;
                external.ops(slot).move_back_to(&mut self.layers[i][j]);
                return true;
            }
            fix += self.indexs[i] * t;
        }
        false
    }
    /// 返回一个完整轮周期的刻度数；具体能否插入以 push 的结果为准。
    pub fn max_time(&self) -> usize {
        N0 * N.pow(L as u32)
    }
    /// 弹出当前首层槽位中的一个任务，空槽返回 None。
    /// 弹出后键失效；需要进入下一刻度时由调用方执行 roll。
    pub fn pop(&mut self, slot: &mut Slot<TimerKey, TimeoutItem<T>>) -> Option<TimeoutItem<T>> {
        self.layer0[self.index].ops(slot).pop_front()
    }
    /// 弹出当前首层槽位中的任务及原键，空槽返回 None。
    /// 返回的键已经失效，不能再用于访问 Slot 或 remove。
    pub fn pop_kv(
        &mut self,
        slot: &mut Slot<TimerKey, TimeoutItem<T>>,
    ) -> Option<(TimerKey, TimeoutItem<T>)> {
        self.layer0[self.index].ops(slot).pop_kv_front()
    }
    /// 前进一个最小刻度，并按需将高层任务分发到低层槽位。
    /// 最外层完成一次回绕时返回 true，否则返回 false。
    /// 调用前应先处理当前槽位中的任务。
    pub fn roll(&mut self, slot: &mut Slot<TimerKey, TimeoutItem<T>>) -> bool {
        // 如果首层的轮没有滚到底，则简单+1返回
        if self.index < N0 - 1 {
            self.index += 1;
            return false;
        }
        self.index = 0;
        // 将后一层的轮上滚动一次，
        self.indexs[0] = (self.indexs[0] + 1) % N;
        // 同一来源槽中的任务可能分散到不同的低层槽，迁移时保持原键不变。
        let source = &mut self.layers[0][self.indexs[0]];
        while !source.head().is_null() {
            let key = source.head();
            let item = slot.get_mut(key).expect("wheel key missing from storage");
            item.timeout -= N0 * self.indexs[0];
            item.index = item.timeout;
            let destination = item.index;
            source
                .ops(slot)
                .move_front_to(&mut self.layer0[destination]);
        }
        if self.indexs[0] > 0 {
            return false;
        }
        // 依次处理每个轮
        for i in 1..L {
            // 将本层的轮上滚动一次，
            self.indexs[i] = (self.indexs[i] + 1) % N;
            let (lower, current) = self.layers.split_at_mut(i);
            let source = &mut current[0][self.indexs[i]];
            let elapsed = N0 * N.pow(i as u32) * self.indexs[i];
            while !source.head().is_null() {
                let key = source.head();
                let item = slot.get_mut(key).expect("wheel key missing from storage");
                item.timeout -= elapsed;
                if item.timeout < N0 {
                    item.index = item.timeout;
                    let destination = item.index;
                    source
                        .ops(slot)
                        .move_front_to(&mut self.layer0[destination]);
                } else {
                    let mut destination = None;
                    for level in 0..i {
                        let t = N0 * N.pow(level as u32);
                        if item.timeout < t * N {
                            let j = item.timeout / t;
                            item.index = N0 + level * N + j;
                            destination = Some((level, j));
                            break;
                        }
                    }
                    let (level, j) = destination.expect("timeout overflow");
                    source.ops(slot).move_front_to(&mut lower[level][j]);
                }
            }
            if self.indexs[i] > 0 {
                // 没有滚到底，则返回false
                return false;
            }
        }
        // 最后一个轮滚到底被重置时, 返回true
        true
    }
    /// 按稳定键移除任务；无效键或外部管理的任务返回 None。
    /// 跨层迁移时键不变，当前槽位从 Slot 中任务的 index 获取。
    /// 必须传入本轮一直使用的 Slot；轮内任务的 index 只允许由轮更新。
    /// 外部任务使用 Wheel::external_index()，由外部结构负责移除。
    pub fn remove(
        &mut self,
        key: TimerKey,
        slot: &mut Slot<TimerKey, TimeoutItem<T>>,
    ) -> Option<TimeoutItem<T>> {
        let index = slot.get(key)?.index;
        if index >= Self::external_index() {
            return None;
        }
        if index < N0 {
            self.layer0[index].ops(slot).remove(key)
        } else {
            let index = index - N0;
            self.layers
                .get_mut(index / N)?
                .get_mut(index % N)?
                .ops(slot)
                .remove(key)
        }
    }

    /// 判断从当前刻度到 timeout 刻度（含）是否可能有任务到期。
    /// timeout 为 0 时只检查当前首层槽位；超出轮周期时检查轮内所有任务。
    /// 高层槽覆盖多个刻度，命中时可能提前返回 true；false 表示范围内没有任务。
    pub fn has_task_by(&self, timeout: usize) -> bool {
        for offset in 0..N0.min(timeout.saturating_add(1)) {
            let j = if offset < N0 - self.index {
                self.index + offset
            } else {
                offset - (N0 - self.index)
            };
            if !self.layer0[j].head().is_null() {
                return true;
            }
        }
        let mut lower_position = self.index;
        let mut width = N0;
        for level in 0..L {
            for distance in 1..=N {
                let earliest = distance * width - lower_position;
                if earliest > timeout {
                    break;
                }
                let j = (self.indexs[level] + distance) % N;
                if !self.layers[level][j].head().is_null() {
                    return true;
                }
            }
            lower_position += self.indexs[level] * width;
            width *= N;
        }
        false
    }
}
/// 保存在共享 Slot 中的定时任务。
pub struct TimeoutItem<T> {
    /// 轮内部使用的时间偏移；外部不应修改。
    pub timeout: usize,
    /// 用户任务数据。
    pub el: T,
    /// 所在槽位的扁平索引；外部管理的任务使用 Wheel::external_index()。
    /// 轮内部据此定位任务，外部不应修改轮内任务的索引。
    pub index: usize,
}
impl<T> TimeoutItem<T> {
    /// 创建具有指定时间偏移、数据和槽位索引的任务。
    pub fn new(timeout: usize, el: T, index: usize) -> Self {
        TimeoutItem { timeout, el, index }
    }
    /// 返回当前保存的时间偏移；任务迁移后可能发生变化。
    pub fn timeout(&self) -> usize {
        self.timeout
    }
}
impl<T: fmt::Debug> fmt::Debug for TimeoutItem<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TimeoutItem")
            .field("timeout", &self.timeout)
            .field("el", &self.el)
            .field("index", &self.index)
            .finish()
    }
}

impl<T> Eq for TimeoutItem<T> {}

impl<T> Ord for TimeoutItem<T> {
    fn cmp(&self, other: &TimeoutItem<T>) -> Ordering {
        self.timeout.cmp(&other.timeout)
    }
}

impl<T> PartialOrd for TimeoutItem<T> {
    fn partial_cmp(&self, other: &TimeoutItem<T>) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> PartialEq for TimeoutItem<T> {
    fn eq(&self, other: &TimeoutItem<T>) -> bool {
        self.timeout == other.timeout
    }
}

#[test]
fn test() {
    use crate::*;
    let mut slot: Slot<TimerKey, TimeoutItem<usize>> = Default::default();
    let vec = vec![
        0, 1, 10, 6, 4, 4, 4, 5, 3, 7, 2, 15, 18, 21, 26, 31, 39, 41, 8, 99, 90,
    ]; //
       //let vec = vec![1,10,6];
    let mut wheel: Wheel<usize, 10, 4, 2> = Wheel::default();
    println!("max_time:{:?}", wheel.max_time());
    for i in vec.clone() {
        wheel.push(i, i, &mut slot);
    }
    println!("{:?}", wheel);
    let mut sorted = vec.clone();
    sorted.sort();
    sorted.reverse();
    let mut c = 0;
    while sorted.len() > 0 {
        if let Some(r) = wheel.pop(&mut slot) {
            println!("r:{:?}, c:{}", r, c);
            assert_eq!(r.el, sorted.pop().unwrap());
            assert_eq!(r.el, c);
            if r.el == 18 {
                sorted.push(32);
                sorted.push(47);
                sorted.sort();
                sorted.reverse();
                wheel.push(14, 32, &mut slot);
                wheel.push(29, 47, &mut slot);
                println!("---{:?}", wheel);
            }
        } else {
            let r = wheel.roll(&mut slot);
            c += 1;
            if c % 10 == 0 {
                if c < 40 {
                    sorted.push(c * 2 + 2);
                    sorted.sort();
                    sorted.reverse();
                    wheel.push(c + 2, c * 2 + 2, &mut slot);
                }
                assert_eq!(c, wheel.roll_count());
                println!("{:?} roll_over:{}", wheel, r);
            }
        }
    }
}

#[test]
fn moves_keep_keys_and_distribute_tasks_across_finer_slots() {
    let mut slot: Slot<TimerKey, TimeoutItem<usize>> = Default::default();
    let mut wheel: Wheel<usize, 3, 2, 2> = Wheel::default();
    let keys = [4, 5, 7, 8, 11].map(|time| match wheel.push(time, time, &mut slot) {
        Result::Ok(key) => key,
        Result::Overflow(_, _) => panic!("unexpected overflow"),
    });
    for tick in 0..=11 {
        for (key, expected) in keys.iter().zip([4, 5, 7, 8, 11]) {
            if tick <= expected {
                assert_eq!(slot.get(*key).map(|item| item.el), Some(expected));
            }
        }
        while let Some((key, item)) = wheel.pop_kv(&mut slot) {
            assert_eq!(item.el, tick);
            assert!(!slot.contains_key(key));
        }
        if tick < 11 {
            wheel.roll(&mut slot);
        }
    }
    assert_eq!(slot.len(), 0);
}

#[test]
fn has_task_by_includes_current_slot_and_covers_coarse_slots() {
    let mut slot: Slot<TimerKey, TimeoutItem<usize>> = Default::default();
    let mut wheel: Wheel<usize, 10, 4, 2> = Wheel::default();
    assert!(!wheel.has_task_by(usize::MAX));
    wheel.push(0, 0, &mut slot);
    assert!(wheel.has_task_by(0));
    wheel.pop(&mut slot);
    wheel.push(11, 11, &mut slot);
    assert!(!wheel.has_task_by(0));
    assert!(!wheel.has_task_by(9));
    assert!(wheel.has_task_by(10));
    wheel.push(80, 80, &mut slot);
    assert!(wheel.has_task_by(usize::MAX));
    for _ in 0..10 {
        wheel.roll(&mut slot);
    }
    assert!(!wheel.has_task_by(0));
    assert_eq!(wheel.pop(&mut slot).map(|item| item.el), None);
    wheel.roll(&mut slot);
    assert_eq!(wheel.pop(&mut slot).map(|item| item.el), Some(11));
}

#[test]
fn has_task_by_finds_high_level_slot_after_multiple_rollovers() {
    let mut slot: Slot<TimerKey, TimeoutItem<usize>> = Default::default();
    let mut wheel: Wheel<usize, 3, 2, 2> = Wheel::default();
    wheel.push(10, 10, &mut slot);
    for _ in 0..4 {
        wheel.roll(&mut slot);
    }
    assert!(!wheel.has_task_by(0));
    assert!(wheel.has_task_by(2));
    assert!(wheel.has_task_by(usize::MAX));
}

#[test]
fn has_task_by_never_misses_scheduled_tasks_across_rollovers() {
    for scheduled in 0..12 {
        let mut slot: Slot<TimerKey, TimeoutItem<usize>> = Default::default();
        let mut wheel: Wheel<usize, 3, 2, 2> = Wheel::default();
        wheel.push(scheduled, scheduled, &mut slot);
        for now in 0..=scheduled {
            assert!(
                wheel.has_task_by(scheduled - now),
                "scheduled={scheduled}, now={now}"
            );
            assert!(wheel.has_task_by(usize::MAX));
            if now < scheduled {
                wheel.roll(&mut slot);
            }
        }
        assert_eq!(wheel.pop(&mut slot).map(|item| item.el), Some(scheduled));
        assert!(!wheel.has_task_by(usize::MAX));
    }
}

#[test]
fn has_task_by_scans_full_wheel_when_timeout_exceeds_capacity() {
    let mut slot: Slot<TimerKey, TimeoutItem<usize>> = Default::default();
    let mut wheel: Wheel<usize, 10, 4, 2> = Wheel::default();
    wheel.push(80, 80, &mut slot);
    assert!(!wheel.has_task_by(39));
    assert!(wheel.has_task_by(80));
    assert!(wheel.has_task_by(usize::MAX));
}

#[test]
fn invalid_wheel_dimensions_are_rejected() {
    assert!(std::panic::catch_unwind(|| Wheel::<(), 0, 2, 1>::default()).is_err());
    assert!(std::panic::catch_unwind(|| Wheel::<(), 2, 0, 1>::default()).is_err());
    assert!(std::panic::catch_unwind(|| Wheel::<(), 2, 2, 0>::default()).is_err());
    assert!(std::panic::catch_unwind(Wheel::<(), 1, 65_536, 4>::validate_dimensions).is_err());
}

#[test]
fn remove_uses_current_slot_after_migration() {
    let mut slot: Slot<TimerKey, TimeoutItem<usize>> = Default::default();
    let mut wheel: Wheel<usize, 3, 2, 2> = Wheel::default();
    let key = match wheel.push(8, 8, &mut slot) {
        Result::Ok(key) => key,
        Result::Overflow(_, _) => panic!("unexpected overflow"),
    };
    let original_index = slot[key].index;
    for _ in 0..6 {
        wheel.roll(&mut slot);
    }
    assert_ne!(slot[key].index, original_index);
    assert_eq!(wheel.remove(key, &mut slot).map(|item| item.el), Some(8));
    assert_eq!(wheel.remove(key, &mut slot).map(|item| item.el), None);
    assert!(!wheel.has_task_by(usize::MAX));
    assert_eq!(slot.len(), 0);
}

#[test]
fn remove_handles_first_layer_and_external_marker() {
    let mut slot: Slot<TimerKey, TimeoutItem<usize>> = Default::default();
    let mut wheel: Wheel<usize, 3, 2, 2> = Wheel::default();
    let key = match wheel.push(0, 7, &mut slot) {
        Result::Ok(key) => key,
        Result::Overflow(_, _) => panic!("unexpected overflow"),
    };
    assert_eq!(wheel.remove(key, &mut slot).map(|item| item.el), Some(7));
    assert!(wheel.is_cur_over());
    let mut external = Deque::new();
    let external_key = external.ops(&mut slot).push_back(TimeoutItem::new(
        100,
        9,
        Wheel::<usize, 3, 2, 2>::external_index(),
    ));
    assert!(wheel.remove(external_key, &mut slot).is_none());
    assert_eq!(slot[external_key].el, 9);
    assert_eq!(
        external.ops(&mut slot).pop_front().map(|item| item.el),
        Some(9)
    );
}

#[test]
fn external_task_enters_wheel_without_changing_key() {
    let mut slot: Slot<TimerKey, TimeoutItem<usize>> = Default::default();
    let mut wheel: Wheel<usize, 3, 2, 2> = Wheel::default();
    let mut external = Deque::new();
    let key = external.ops(&mut slot).push_back(TimeoutItem::new(
        999,
        17,
        Wheel::<usize, 3, 2, 2>::external_index(),
    ));
    assert!(!wheel.move_from_external(12, key, &mut external, &mut slot));
    assert_eq!(external.len(), 1);
    wheel.roll(&mut slot);
    assert!(wheel.move_from_external(8, key, &mut external, &mut slot));
    assert_eq!(external.len(), 0);
    assert_eq!(slot[key].el, 17);
    assert_eq!(wheel.remove(key, &mut slot).map(|item| item.el), Some(17));
    assert_eq!(slot.len(), 0);
}

#[test]
fn external_task_entering_first_layer_uses_new_timeout() {
    let mut slot: Slot<TimerKey, TimeoutItem<usize>> = Default::default();
    let mut wheel: Wheel<usize, 3, 2, 2> = Wheel::default();
    let mut external = Deque::new();
    let key = external.ops(&mut slot).push_back(TimeoutItem::new(
        999,
        42,
        Wheel::<usize, 3, 2, 2>::external_index(),
    ));
    assert!(wheel.move_from_external(2, key, &mut external, &mut slot));
    assert_eq!(slot[key].timeout, 2);
    wheel.roll(&mut slot);
    wheel.roll(&mut slot);
    assert_eq!(
        wheel.pop_kv(&mut slot).map(|(k, item)| (k, item.el)),
        Some((key, 42))
    );
    assert_eq!(slot.len(), 0);
}

#[test]
fn oversized_timeout_returns_overflow_without_panicking() {
    let mut slot: Slot<TimerKey, TimeoutItem<usize>> = Default::default();
    let mut wheel: Wheel<usize, 3, 2, 2> = Wheel::default();
    wheel.roll(&mut slot);
    match wheel.push(usize::MAX, 17, &mut slot) {
        Result::Overflow(time, value) => {
            assert_eq!(time, usize::MAX);
            assert_eq!(value, 17);
        }
        Result::Ok(_) => panic!("unexpected insertion"),
    }
    assert_eq!(slot.len(), 0);
}
