# pi_cancel_timer

基于多层时间轮与最小堆的可取消定时器。近期任务放在时间轮，超出轮覆盖范围的任务放在堆中；取消远期任务时立即释放任务值，堆中的索引惰性清理，并在积累过多时批量回收。

## 时钟与精度

所有传入的时间都是**同一外部时钟原点下的 `u64` 毫秒值**。库不读取系统时间；调用方负责提供起始时间、截止时间以及弹出时的当前时间。比如可以使用同一 `Instant` 起点计算毫秒数，但不能混用不同起点的时间。

`Timer<T, N0, N, L, PRECISION_MS>` 的 `N0` 是首层槽数，`N` 是其余各层槽数，`L` 是其余层数；完整轮周期是 `N0 * N^L` 个刻度。末尾的 `PRECISION_MS` 是每刻度毫秒数，省略时为 `1`，不能为零。维度无效或容量溢出时，构造会 panic。

`Timer::new(start_ms)` 以 `start_ms` 作为第零刻度；`Default::default()` 等同于 `new(0)`。向将来安排的截止时间**向上对齐**到可表示的刻度，传给 `pop` / `has_task_by` 的时间**向下取整**到当前可达刻度。例如起点为 101 ms、精度为 10 ms，安排 116 ms 截止的任务会在 121 ms 刻度触发，120 ms 不会提前弹出。对齐后的截止时间超出 `u64` 时会 panic，不插入任务。

## 使用示例

```rust
use pi_cancel_timer::Timer;

// 四个首层槽、一个拥有两个槽的高层；每刻度为 10 毫秒。
let mut timer: Timer<&str, 4, 2, 1, 10> = Timer::new(101);
let early = timer.schedule_at(116, "到期任务");
let cancelled = timer.schedule_after(100, "待取消任务");

assert_eq!(timer.count(), 2);
assert_eq!(timer.cancel(cancelled), Some("待取消任务"));
assert_eq!(timer.count(), 1);
assert!(!timer.has_task_by(120)); // 只读预判，不推进内部时间
assert_eq!(timer.pop(120), None);
assert_eq!(timer.current_time_ms(), 111);
assert_eq!(timer.pop(121), Some("到期任务"));
assert_eq!(timer.cancel(early), None); // 已弹出的键不再有效
assert_eq!(timer.count(), 0);
```

## 方法语义

| 方法 | 行为 |
| --- | --- |
| `new(start_ms)` | 指定调用方时钟的起点；不读取系统时钟。 |
| `current_time_ms()` | 返回内部当前刻度的绝对毫秒时间。只有 `pop` 会推进它；它可能落后于调用方传入的时间。 |
| `schedule_at(deadline_ms, value)` | 按同源绝对截止毫秒安排任务；过去或当前截止时间放入当前刻度。 |
| `schedule_after(delay_ms, value)` | 相对**内部当前刻度**安排任务，不是相对真实墙上时钟。 |
| `pop(now_ms)` | 先弹内部当前刻度任务；若为空，则按需逐刻度推进，弹出截至目标刻度的一项任务。可重复调用，直到返回 `None`。 |
| `has_task_by(now_ms)` | 只读预判是否可能有截至该时刻的任务；`false` 表示没有，`true` 仍需以 `pop` 为准。 |
| `cancel(key)` | 取消未弹出的任务并返回任务值；无效、已取消或已弹出的键返回 `None`。 |
| `count()` | 当前有效任务总数，包含轮内和远期任务；不计入已取消的堆索引。 |

`pop(now_ms)` 不会让内部时间倒退；如果内部已推进，而随后安排了过去截止时间的任务，该任务会放在当前刻度，即使下次传入更早的 `now_ms` 也能弹出。

时间轮的 `has_task_by` 对高层粗粒度槽可能提前报告 `true`；定时器同时查询轮内和有效堆任务。它不会推进时钟或清理失效堆索引。远期任务迁入轮时保持原取消键。堆的失效条目达到至少 64 个且占堆至少一半时，会过滤并重建堆，避免大量远期取消索引长期滞留。
