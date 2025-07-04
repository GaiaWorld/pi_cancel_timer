//! 可撤销的定时器

use std::{cmp::Reverse, fmt};

use pi_ext_heap::ExtHeap;
use pi_slot_deque::{LinkedNode, Slot};
use pi_slot_wheel::{Result, TimeoutItem, TimerKey, Wheel};
use slotmap::{Key};


/// 可撤销的定时器
pub struct Timer<T, const N0: usize, const N: usize, const L: usize> {
    slot: Slot<TimerKey, TimeoutItem<T>>,
    wheel: Wheel<T, N0, N, L>, // 定时轮
    heap: ExtHeap<Reverse<(usize, TimerKey)>>, // 最小堆
    add_count: usize,
    remove_count: usize,
    roll_count: u64,
}

impl<T: fmt::Debug, const N0: usize, const N: usize, const L: usize> fmt::Debug
    for Timer<T, N0, N, L>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Timer")
            .field("wheel", &self.wheel)
            .field("heap", &self.heap)
            .field("add_count", &self.add_count)
            .field("remove_count", &self.remove_count)
            .field("roll_count", &self.roll_count)
            .finish()
    }
}
impl<T, const N0: usize, const N: usize, const L: usize> Default for Timer<T, N0, N, L> {
    fn default() -> Self {
        Timer {
            slot: Default::default(),
            wheel: Default::default(),
            heap: Default::default(),
            add_count: 0,
            remove_count: 0,
            roll_count: 0,
        }
    }
}

impl<T, const N0: usize, const N: usize, const L: usize> Timer<T, N0, N, L> {
    /// 获得添加任务数量
    pub fn add_count(&self) -> usize {
        self.add_count
    }
    /// 获得移除任务数量
    pub fn remove_count(&self) -> usize {
        self.remove_count
    }
    /// 获得滚动次数
    pub fn roll_count(&self) -> u64 {
        self.roll_count
    }

     /// 在当前时间之后，放入一个定时任务
    pub fn push_time(&mut self, time: u64, el: T) -> TimerKey {
        self.push(match time.checked_sub(self.roll_count) {
            Some(r) => r as usize,
            _ => 0,
        }, el)
    }

    /// 放入一个定时任务
    pub fn push(&mut self, timeout: usize, el: T) -> TimerKey {
        self.add_count += 1;
        match self.wheel.push(timeout, el, &mut self.slot) {
            Result::Ok(key) => key,
            Result::Overflow(timeout, el) => {
                // 将定时任务放入slot中
                let key = self.slot.insert(LinkedNode::new(
                    TimeoutItem::new(0, el, N0 + N * L),
                    TimerKey::null(),
                    TimerKey::null(),
                ));
                // 将绝对时间和键放入堆中
                let loc = self.heap.push(
                    Reverse((timeout, key)),
                    &mut self.slot,
                    set_index::<T, N0, N, L>,
                );
                // 修正所在的堆位置
                // unsafe {
                //     self.slot.get_unchecked_mut(key).el.index += loc;
                // }
                key
            }
        }
    }
    /// 弹出定时间内的一个定时任务
    /// * @return `Option<T>` 弹出的定时元素
    pub fn pop(&mut self, now: u64) -> Option<T> {
        loop {
            if let Some(r) = self.wheel.pop(&mut self.slot) {
                self.remove_count += 1;
                return Some(r.el)
            }
            if self.roll_count >= now {
                return None
            }
            self.roll();
        }
    }
    /// 弹出定时间内的一个关键字和定时任务
    /// * @return `Option<(TimerKey, T)>` 弹出的关键字和定时元素
    pub fn pop_kv(&mut self, now: u64) -> Option<(TimerKey, T)> {
        loop {
            if let Some((key, r)) = self.wheel.pop_kv(&mut self.slot) {
                self.remove_count += 1;
                return Some((key, r.el))
            }
            if self.roll_count >= now {
                return None
            }
            self.roll();
        }
    }
    /// 判断指定时间内是否还有定时任务
    pub fn is_ok(&mut self, now: u64) -> bool {
        loop {
            if !self.wheel.is_cur_over() {
                return true
            }
            if self.roll_count >= now {
                return false
            }
            self.roll();
        }
    }
    /// 轮滚动 - 向后滚动一个最小粒度, 可能会造成轮的逐层滚动。如果滚动到底，则修正堆上全部的定时任务，并将堆上的到期任务放入轮中
    pub fn roll(&mut self) {
        self.roll_count += 1;
        if self.wheel.roll(&mut self.slot) {
            // 修正堆上全部的定时任务
            for i in 0..self.heap.len() {
                unsafe { self.heap.get_unchecked_mut(i).0.0 -= self.wheel.max_time() };
            }
            // 如果滚到轮的最后一层的最后一个， 则将堆上的到期任务放入轮中
            // 检查堆顶的最近的任务
            while let Some(it) = self.heap.peek() {
                // 判断任务是否需要放入轮中
                if it.0.0 >= self.wheel.max_time() {
                    break;
                }
                let Reverse((mut timeout, key)) = self
                    .heap
                    .pop(&mut self.slot, set_index::<T, N0, N, L>)
                    .unwrap();
                // 时间已经修正过了，可以直接放入定时轮中
                self.wheel
                    .push_key(key, &mut self.slot, &mut timeout, retimeout);
            }
        }
    }
    /// 取消定时任务
    pub fn cancel(&mut self, key: TimerKey) -> Option<T> {
        match self.slot.remove(key) {
            Some(node) => {
                self.remove_count += 1;
                if node.el.index < N0 + N * L {
                    self.wheel.get_slot_mut(node.el.index).repair(
                        node.prev(),
                        node.next(),
                        &mut self.slot,
                    );
                } else {
                    self.heap.remove(
                        node.el.index - N0 - N * L,
                        &mut self.slot,
                        set_index::<T, N0, N, L>,
                    );
                }
                Some(node.el.el)
            }
            _ => None,
        }
    }
}
fn retimeout<T>(timeout: &mut usize, it: &mut TimeoutItem<T>) {
    it.timeout = *timeout;
}
fn set_index<T, const N0: usize, const N: usize, const L: usize>(
    slot: &mut Slot<TimerKey, TimeoutItem<T>>,
    arr: &mut [Reverse<(usize, TimerKey)>],
    loc: usize,
) {
    let i = &arr[loc];
    unsafe {
        slot.get_unchecked_mut(i.0 .1).el.index = N0 + N * L + loc;
    }
}


// 测试定时器得延时情况
#[cfg(test)]
mod test_mod {
    extern crate pcg_rand;
    extern crate rand_core;

    use std::{
        thread,
        time::{Duration, Instant},
    };

    use self::rand_core::{RngCore, SeedableRng};
    use crate::*;

    #[test]
    fn test() {
        let mut timer: Timer<(u64, u64), 128, 16, 1> = Default::default();
        let mut rng = pcg_rand::Pcg32::seed_from_u64(22222);
        let start = Instant::now();
        println!("max_time:{}", timer.wheel.max_time());
        for i in 1..100000 {
            let t = (rng.next_u32() % 16100) as u64;
            let now = Instant::now();
            let tt = now.duration_since(start).as_millis() as u64;
            if i < 100 {
                println!("push: timeout:{} realtime:{:?}", t, (i, t + tt));
                timer.push(t as usize, (i, t + tt));
            }
            if t == 9937 || t == 15280 {
                println!("{:?}", timer.wheel);
            }
            //while let Some(it) = timer.pop(tt) {
            while timer.is_ok(tt) {
                let it = timer.pop(tt).unwrap();
                println!("ppp:{:?}, now:{}", it, tt);
            }
            if i > 100 && timer.add_count == timer.remove_count {
                //println!("vec:{:?}", vec);
                println!(
                    "return: add_count:{:?}",
                    timer.add_count
                );
                return;
            }
            thread::sleep(Duration::from_millis(1 as u64));
        }
    }

}
