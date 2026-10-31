# GC 测例编写注意点（tests/gc）

> 面向 `tests/gc/*.html` 这类「用 `window.runtime` 观察 QuickJS 引用计数」的手工测例。
> 最近核对：2026-10-31。对象计数机制见 `servo-quickjs-GC.md` 和 `servo-quickjs-GC-pool接口调用点.md`。

## 1. 先记住三件事

1. **计数是 servo 侧和 JS 侧共享的**：JS 多一个变量指向 DOM 节点，计数就 +1。
2. **测例要保证两次读数的「JS 侧口径」一致**，否则比较没有意义。
3. **读数是引擎的引用计数，不只是 JS 变量个数**；它包含 Servo 侧 `Dom<T>`、布局、pool 等所有引用。

## 2. 可用接口（仅 js-quickjs 构建）

`window.runtime` 是调试钩子，不是 Web API：

| 接口 | 作用 |
|---|---|
| `runtime.qjs_refcount(node)` | 读节点当前引用计数；已减去传参时 VM 栈上的那份临时 +1 |
| `runtime.drain_pool()` | 立刻 drain autorelease pool |
| `runtime.qjs_gc()` | 触发一次 QuickJS GC（`JS_GC`，`GCReason::API`） |

实现见 `components/script_bindings/quickjs_runtime_helper.rs`。

## 3. 必须用带 runtime 的二进制

1. `target/debug/servoshell` 可能没有编入 `runtime`，会报
   `cannot read property 'drain_pool' of undefined`。
2. **用 `target/pool-stats/debug/servoshell`**（或用 `event_flame.sh` 的构建方式）。
3. 运行命令：

```sh
./target/pool-stats/debug/servoshell -z --exit "file://$PWD/tests/gc/4-remove-drain-append.html"
```

4. 输出里会跟一大段 leak dump，过滤结果行：

```sh
... 2>&1 | grep -E "^\[task|^\[PASS|^\[FAIL|Error at"
```

## 4. JS 侧引用控制（最容易出错）

1. **节点只留一个 JS 变量**（例如 `var node`），两次测量时它都在，双方各 +1，口径一致。
2. **API 返回值必须接住再显式置 null**：
   ```js
   tmp = parent.appendChild(node);
   tmp = null;   // 显式丢弃返回值那份引用
   ```
   `removeChild` 同样处理。不接返回值会留下 pool/栈上的临时引用，读数不稳。
3. **`console.log` 只打印数字**，不要打印节点本身（打印会制造临时引用）。
4. **不要**用数组、对象、`Map`、二次变量缓存节点；每多一处持有就多 +1。
5. 闭包捕获的是**变量绑定**，不会额外复制值；但仍不要为了「方便」多写变量。
6. 节点下树后用 `getElementById` 找不到，所以跨 task 只能靠一个 JS 变量持有。

## 5. 读数前的固定动作（顺序照抄）

上树、下树、重新上树三个测量点都做**同一套动作**：

```js
tmp = parent.appendChild(node);   // 或 removeChild
tmp = null;                       // 显式丢弃返回值
void node.offsetHeight;           // 强制 reflow，让布局侧引用先建立/释放
window.runtime.drain_pool();      // 清掉池尾，读数才稳定
// 下树之后额外加：
window.runtime.qjs_gc();          // 尝试让 QuickJS GC 误回收
var count = window.runtime.qjs_refcount(node);
```

要点：

1. **reflow 不能只做上树那一次**：上树后、下树后、重新上树后都要做。
2. **drain 后再读数**；否则 pool 里的临时引用会让数字随机波动。
3. **下树后加 `qjs_gc()`**：测例的目的就是看 GC 会不会误回收下树对象。
4. 若 `qjs_gc()` 前后计数变了，说明 GC 收走了东西——这才是要找的 bug。

## 6. 「另一个 task」怎么表达

1. **用 `setTimeout(fn, 0)`**，不要用 `Promise.then` / `queueMicrotask`。
2. 原因：`setTimeout` 是 **task**（timer task source），代码里显式 `task_source.queue(task!(...))`
   （`components/script/event_loop/timers.rs`）；微任务是另一条队列，不算新 task。
3. pool 的 drain 在每轮 `handle_msgs` 之后（`components/script/event_loop/script_thread.rs:1039`），
   粒度是「一批 task 一次」，不严格等于每个 spec task。
4. 实测中 setTimeout 会由 timer 线程单独唤醒，所以 detach 和 reattach 之间确实隔了一次 drain。

## 7. 读数怎么解释

1. **下树后计数 > 1 不一定是 bug**：至少还有 JS 变量那 1 份。
2. 其余来自 Servo 侧结构引用，例如 `node.textContent = "..."` 创建的**文本子节点**，
   它的 `parent` 指针会反向引用 div，+1；下树时跟着节点走，不会断。
3. 已有测例的实测基线：

| 测例 | 上树前 | 下树后（drain+gc） | 重新上树后 | 结果 |
|---|---|---|---|---|
| `4-remove-drain-append.html`（有 textContent） | 4 | 2 | 4 | PASS |
| `5-remove-drain-append-no-textContent.html`（无） | 3 | 1 | 3 | PASS |

4. 两个测例每步正好差 1，说明差异就是那棵文本子树的 parent 引用。
5. 想看清「纯 JS 引用基线」，用不带 textContent 的版本：下树后应为 1。

## 8. 判定与输出约定

1. 判据：**下树前计数 === 重新上树后计数**。
2. 用固定前缀输出，便于 grep：`[task1] ...`、`[task2] ...`、`[PASS] ...`、`[FAIL] ...`。
3. `[FAIL]` 时把两个数值都打印出来，不要只打印布尔。
4. 下树后的计数只作诊断，不参与判据。

## 9. 常见坑

1. 用 `target/debug/servoshell` 跑 → `runtime` undefined。
2. 忘了 `tmp = null` → 读数比预期高 1。
3. 只在上树时 reflow → 下树/重上树的数字对不上。
4. 测量前打印节点 → 多出临时引用。
5. 用微任务代替 `setTimeout` → 不是真正的另一个 task。
6. 节点下树后用 `getElementById` 取回 → 找不到（不在文档树里）。
7. 两次测量时 JS 变量数量不同 → 计数天然对不上，不是 GC 的问题。
