# macOS `release_pointer` 作用域过宽

- **状态**：已知隐患，暂不修复
- **影响平台**：仅 macOS
- **当前实际影响**：无
- **触发条件**：overlay 进程新增第二个窗口时
- **相关文件**：`crates/openlogi-overlay/src/platform.rs`
- **发现于**：对比 `a92aa43b`（v0.8.6）审查操作环改动时

---

## 现象

`release_pointer` 的 macOS 实现拿到了调用方传入的目标窗口，却没有使用它，
转而对**当前进程的所有 NSWindow** 执行了操作：

```rust
#[cfg(target_os = "macos")]
pub(crate) fn release_pointer(_window: &gpui::Window) {
    //                         ^ 传入的目标窗口被忽略
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    if let Some(marker) = MainThreadMarker::new() {
        for window in NSApplication::sharedApplication(marker).windows() {
            window.setIgnoresMouseEvents(true);
        }
    }
}
```

对照同一函数的 Windows 实现（`platform/windows/native.rs`），那里是通过
`raw_window_handle` 精确取出目标 HWND 后只改那一个窗口的扩展样式。两个平台
的实现对同一个契约给出了不同的作用域。

## 这个函数的职责

操作环退场动画期间（约 150ms）窗口仍在屏幕上，但已不再是交互对象。
`release_pointer` 负责让这段时间内的点击穿透过去，落到用户真正想点的应用上，
而不是被正在消失的环截住。

## 为什么现在没有问题

overlay 进程当前只持有操作环一个窗口，"所有窗口"与"那一个窗口"是同一个集合，
因此遍历写法与精确写法行为完全一致。这不是一个正在发生的 bug。

## 将来会怎样出问题

一旦 overlay 进程开出第二个窗口（例如 hover 提示气泡、设置浮层），该窗口会在
操作环退场时被一并设为忽略鼠标事件，且调用点与故障点无任何可见关联。

排查代价很高：改动只涉及"环的退场逻辑"，症状却出现在另一个不相关的窗口上。

## 正确修法

```rust
#[cfg(target_os = "macos")]
pub(crate) fn release_pointer(window: &gpui::Window) {
    use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
    let Ok(handle) = window.window_handle() else { return };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else { return };
    // handle.ns_view: NonNull<c_void> → &NSView → NSView::window()
    //   → setIgnoresMouseEvents(true)
}
```

## 为什么暂不执行

1. **需要新增依赖**。`raw-window-handle` 目前只在 overlay 的
   `[target.'cfg(target_os = "windows")'.dependencies]` 段内，macOS 段没有。
   按 `AGENTS.md`，overlay / `openlogi-ui` 的依赖新增有既定约束，不是随手可加。

2. **需要编写 unsafe FFI**。从 `ns_view` 裸指针反查 `NSView::window()` 必须
   跨越一次 `unsafe` 指针转换。此类错误不在编译期暴露，而是运行时崩溃。

3. **无验证环境**。本仓库当前无 macOS 机器。在完全无法运行验证的前提下提交
   一段 unsafe 的 AppKit 调用，风险与收益不成比例——尤其该隐患当前实际影响为零。

另见 `.agents/rules/objc-ffi.md`：macOS 原生 FFI 是有专属契约的受控面，
修改前需按该规则登记。

## 处理建议

择一：

- **A（推荐）**：保持现状，待有 macOS 环境的贡献者接手，或推迟到真正需要新增
  第二个 overlay 窗口的那次改动中一并处理。彼时该改动的作者必然具备验证条件。

- **B（零依赖折中）**：不改依赖、不写 unsafe，仅在函数入口处对窗口数量做一次
  断言或告警日志——进程窗口数大于 1 时记录警告。代价接近于零，且能在隐患真正
  被触发的第一时间把问题定位到此处，而不是让排查者从受害窗口反向摸索。

## 验收条件

修复后应满足：

- `release_pointer` 使用传入的 `window` 参数，不再遍历 `NSApplication.windows()`
- macOS 与 Windows 两个实现的作用域语义一致（均为"仅目标窗口"）
- 在 macOS 上验证：操作环退场期间点击可穿透，且同进程其他窗口不受影响
