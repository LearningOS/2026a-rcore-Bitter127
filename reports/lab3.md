# Lab 3：进程管理与调度

## 实验内容

本章在已有的 fork/exec/wait 基础上实现 `spawn`，并把就绪队列从普通 FIFO 改为 stride 调度。地址空间相关的 `get_time`、`mmap` 和 `munmap` 也需要适配新的进程控制块。

## spawn

`spawn` 直接根据程序名找到 ELF 数据，再创建一个全新的进程控制块。新进程拥有自己的地址空间、PID 和内核栈，同时记录父进程的弱引用；父进程的 children 列表保存子进程，之后 `waitpid` 才能正确找到并回收它。程序名不存在时直接返回 `-1`，不会把半初始化的进程放进调度队列。

## stride 调度

每个进程保存 priority 和 stride。调度器每次选择 stride 最小的就绪进程，选中后按 `BIG_STRIDE / priority` 增加它的 stride，因此 priority 越大，增长越慢，在相同时间内被选中的次数越多。我使用 `u128` 保存 stride，避免长时间运行时很快发生整数回绕。priority 小于 2 时返回 `-1`，fork 出来的子进程继承父进程当前的调度参数。

## 验证结果

使用课程工具链完成 RISC-V release 构建，并在 QEMU 8.2.2、RustSBI 0.4.1 上执行官方测试。`ch5.py` 返回 `15/15`，spawn、wait/waitpid 和 set_priority 均通过。额外运行 stride 测试时，优先级 5～10 的计数随优先级上升，程序最终输出 `ch5 Usertests passed!`。
