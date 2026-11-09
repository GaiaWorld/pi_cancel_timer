# pi_cancel_timer

基于多层时间轮（timing wheel）与最小堆（min-heap）的可取消定时器（cancellable timer）。所有任务（task）保存在共享存储（shared storage）中，近期任务（task）由时间轮（timing wheel）组织，远期任务（task）由外部队列（external queue）组织；堆（heap）只保存远期任务（task）的到期时间（deadline）与键（key）。取消（cancel）远期任务（task）时立即释放任务值（task value），堆（heap）中的索引（index）采用惰性清理（lazy cleanup），并在失效条目（stale entry）积累过多时过滤（filter）并重建（rebuild）堆（heap）。

## 0.7 版本兼容性（version compatibility）

本次公开接口（API）重构（refactoring）包含破坏性变更（breaking change），以 `pi_cancel_timer 0.7.0` 发布（publish）。采用 `pi_cancel_timer = "0.4"`、`"0.5"` 或 `"0.6"` 的下游依赖约束（dependency requirement）不会自动选择 `0.7.0`；使用更宽版本范围（version range）的调用方（caller）需自行核对升级范围。

迁移（migration）到 `pi_cancel_timer = "0.7"` 时，需要调整旧接口（API）的调用（call）：

| 原 `nightly-1.98.0` 的旧接口（API） | 0.7 中的迁移要求（migration requirement） |
| --- | --- |
| `push(timeout: usize, value)` | 已移除；使用 `schedule_after(delay_ms: u64, value)`，时间单位明确为毫秒（millisecond）。 |
| `push_time(time: u64, value)` | 已移除；使用 `schedule_at(deadline_ms: u64, value)`，截止时间（deadline）与起点必须来自同一时钟（clock）。 |
| `pop_kv(now)` | 已移除；`pop(now_ms)` 只返回任务值（task value），不返回键（key）。 |
| `is_ok(now)`、`is_null(time)` | 已移除；`has_task_by(now_ms)` 不推进内部时间（internal time），并允许提前返回 `true`，不能等价替换旧查询（query）后继续假定 `pop` 必定有值。 |
| `add_count()`、`remove_count()` | 累计统计（cumulative statistics）已移除；`count()` 仅表示当前有效任务数（task count）。 |
| `roll_count()`、公开的 `roll()` | 已移除；`current_time_ms()` 返回绝对时间（absolute time）的毫秒值（millisecond value），内部时间（internal time）由 `pop` 推进。 |

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

## 轮外任务（task）的存储（storage）与迁入（migration）

剩余延迟（remaining delay）小于 `N0 * N^L` 个刻度（tick）的任务（task）直接进入时间轮（timing wheel）；大于或等于该范围的任务（task）进入 `external` 队列（queue），并在堆（heap）中保存对齐后的绝对截止时间（aligned absolute deadline）与 `TimerKey`，时间单位为毫秒（millisecond）。两类队列（queue）共享 `slot`，每项任务（task）只保存一份。

`pop` 推进内部时间（internal time）时，每当整个多层时间轮（timing wheel）完成一个周期（cycle），便检查堆（heap）顶端，将剩余刻度（tick）小于轮容量（wheel capacity）的任务（task）通过 `move_from_external` 按键（key）迁入（migrate）时间轮（timing wheel）；无需等待任务（task）到期。堆（heap）按截止时间（deadline）排序（sort），外部队列（external queue）按插入顺序（insertion order）组织，因此要迁移（migrate）的键（key）不一定是队尾键（tail key）。迁移（migration）保持原取消键（cancellation key），后续仍可用它取消（cancel）任务（task）。

## 0.7 依赖迁移（dependency migration）

使用 `pi_slot_wheel = "0.7"` 和 `pi_slot_deque = "0.6"`，间接依赖（transitive dependency）使用 `pi_slotmap = "0.6"`；时间轮（timing wheel）和槽映射（slotmap）统一使用 `pi_key = "0.4"`。直接声明这些依赖（dependency）或使用返回键（key）的调用方（caller）应同步升级，避免混用不同版本的键类型（key type）和特征（trait）。相对主分支（master）的 `0.6.0`，`Timer` 的公开方法（public method）调用方式不变；相对原 `nightly-1.98.0` 的旧接口（API），仍须按前表迁移（migration）。

保留主分支（master）的按键迁移修正（key-based migration fix）：多个远期任务（task）的到期顺序与插入顺序不同时，仍按正确的键（key）迁入时间轮（timing wheel），并保持取消键（cancellation key）有效。

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

## Rust 工具链（toolchain）与依赖（dependency）

本分支面向 Rust `1.98.0-nightly` 及后续版本。可用对应工具链（toolchain）执行 `cargo build --all-targets --all-features`、`cargo test --all-features` 及 `cargo build --release --all-features`；本次已验证 Rust `1.98.0-nightly` 和 `1.99.0-nightly`。

以下依赖约束（dependency requirement）使用 crates.io 正式发布版本（release version），仅指定主版本号（major version）与次要版本号（minor version）：

```toml
[dependencies]
pi_slot_wheel = "0.7"
pi_slot_deque = "0.6"
```

`pi_slotmap` 由 `pi_slot_deque` 间接引入，其依赖约束（dependency requirement）为 `"0.6"`，无需在本库重复声明。本次解析结果（resolution）为 `pi_slot_wheel 0.7.0`、`pi_slot_deque 0.6.0`、`pi_slotmap 0.6.0`。后续更新修订版本（patch version）时，应重新核对实际解析结果（resolution）和接口兼容性（API compatibility）。
