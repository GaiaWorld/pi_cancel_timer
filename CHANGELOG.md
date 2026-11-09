# 更新记录

## 0.7.0

- 合并（merge）主分支（master）`3337c38`，保留轮外任务（task）按键（key）迁入的修正和回归测试（regression test）。
- 使用正式依赖（dependency）`pi_slot_wheel = "0.7"`、`pi_slot_deque = "0.6"`，间接依赖（transitive dependency）使用 `pi_slotmap 0.6.0`，移除随库源码副本（vendored copy）。
- 提升次要版本号（minor version），隔离原 `nightly-1.98.0` 接口（API）重构（refactoring）和依赖（dependency）升级的破坏性变更（breaking change），补充迁移说明（migration guide）。
- 保持 Rust `1.98.0-nightly` 及后续版本的兼容性（compatibility）；本次验证覆盖 Rust `1.98.0-nightly` 和 `1.99.0-nightly`。

## 0.6.0

- 升级依赖至 `pi_slot_wheel = "0.6"`、`pi_slot_deque = "0.5"`，引入轮外任务按指定键迁移的修复。
- 公开方法返回和接收的键关联新版 `pi_key = "0.4"`；直接使用相关依赖的调用方应同步升级。方法调用方式不变。
- 增加多个远期任务按截止时间迁移的回归测试，验证触发顺序、稳定取消键和任务计数。
- 补充共享存储、外部队列、堆索引和完整轮周期迁入机制说明。
