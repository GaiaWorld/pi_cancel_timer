# pi_slot_wheel

基于常量泛型的多层定时轮。调用方负责推进时间，轮内任务跨层迁移时保持原键不变。

## 参数与存储

`Wheel<T, N0, N, L>` 中，`N0` 是首层槽数，`N` 是其余各层的槽数，`L` 是其余层数。例如 `Wheel<u32, 10, 4, 2>` 包含 10 个首层槽和两层各 4 个高层槽。三者都必须大于零，且 `N0 * N^L` 不能超出 `usize`；否则构造时会 panic。`max_time()` 返回这一完整轮周期的刻度数，`push` 是否能容纳特定延时以其返回值为准。

定时轮不拥有任务存储。调用方创建一份 `Slot<TimerKey, TimeoutItem<T>>`，并在该轮的所有 `push`、`pop`、`pop_kv`、`roll` 和 `remove` 调用中传入**同一份存储**。非空轮不能换用另一份存储。`TimerKey` 在轮内迁移期间有效；任务被弹出或移除后失效。`TimeoutItem.timeout` 和 `TimeoutItem.index` 是轮的内部状态，不要修改轮内任务的这两个字段。

## 基本用法

`timeout` 表示从当前刻度开始的延时，`0` 表示当前槽。先处理当前槽中的任务，再调用 `roll` 前进一个刻度：

```rust
use pi_slot_wheel::{Result as PushResult, Slot, TimeoutItem, TimerKey, Wheel};

let mut wheel = Wheel::<&str, 10, 4, 2>::default();
let mut slot = Slot::<TimerKey, TimeoutItem<&str>>::new();

let key = match wheel.push(12, "任务", &mut slot) {
    PushResult::Ok(key) => key,
    PushResult::Overflow(_, _) => panic!("定时时间超出范围"),
};

// 键可以用于提前取消任务；即使任务已经跨层迁移，也不需要更新键。
assert_eq!(slot.get(key).unwrap().el, "任务");
for _ in 0..12 {
    assert!(wheel.pop(&mut slot).is_none());
    wheel.roll(&mut slot);
}
assert_eq!(wheel.pop(&mut slot).unwrap().el, "任务");
assert!(slot.get(key).is_none()); // 弹出后键已失效。
```

`push` 超出可容纳范围时返回 `Result::Overflow(修正时间, 原任务)`，不插入任务；修正时间计算溢出时饱和为 `usize::MAX`。

## 查询与取消

| 方法 | 行为 |
| --- | --- |
| `is_cur_over()` | 当前首层槽为空时返回 `true`。 |
| `has_task_by(timeout)` | 从当前刻度到指定刻度（含）是否**可能**有任务；`timeout=0` 只查当前槽。高层槽覆盖多个刻度，`true` 可能是提前命中，`false` 表示范围内没有任务；超出轮周期时查询轮内所有槽。 |
| `pop(slot)` | 弹出当前槽中的一个 `TimeoutItem<T>`，键随之失效。 |
| `pop_kv(slot)` | 弹出当前槽中的任务及原键；返回的键已经失效。 |
| `remove(key, slot)` | 按稳定键取消轮内任务，返回任务；无效键或外部管理的任务返回 `None`。 |
| `roll(slot)` | 前进一个刻度并逐层分发任务；最外层完成一次回绕时返回 `true`。 |
| `roll_count()` | 返回轮指针在当前完整周期内的位置，回绕后从零开始。 |

需要把任务交给外部结构（例如外部堆）管理时，其 `TimeoutItem.index` 可使用 `EXTERNAL_INDEX`。轮的 `remove` 遇到该标记返回 `None`，**不会**从外部结构或共享存储中删除任务；外部结构必须自行管理这些任务的移除。不要给轮内任务设置这个标记。
