# JS_GC 基线（2026-11-02）

**问题**：dom-modify 及两个更重的 Dromaeo 页面，在直接加载时是否触发 `JS_GC`？

**结论（事实）**：三个页面都没有触发。`qjs-gc:` 行数均为 0。

## 命令

```sh
cd /home/l00982027/mygit/qjs-diff
for page in dom-modify dom-traverse jslib-modify-jquery; do
  timeout -s KILL 30 env QJS_GC_STATS=1 QJS_POOL_STATS_SAMPLE=0 \
    ./servo-ohos/target/pool-stats/debug/servoshell --headless \
    ./servo-ohos/tests/dromaeo/dromaeo/web/tests/$page.html \
    > /tmp/qjs-gc-$page.out 2> /tmp/qjs-gc-$page.log
  echo "$page: $(grep -c '^qjs-gc:' /tmp/qjs-gc-$page.log)"
done
```

输出：

```text
dom-modify: 0
dom-traverse: 0
jslib-modify-jquery: 0
```

## 判定

**全部为 0 → 跳过计划中的 Task 3（给 `JS_GC` 计时），只做 pool 侧。**
如果以后要评估引擎 GC 暂停，先换用完整的 Dromaeo 运行器重新做这个基线。

## 限制（必须一起读）

1. **直接加载 = 每个测试只跑一次**。`htmlrunner.js` 在没有父窗口时退化为
   `test(name, fn){ fn(); }`，`startTest` / `endTest` 都是空函数。所以这里测到的是一次执行，
   不是 Dromaeo 运行器下的重复循环。
2. **`QJS_GC_STATS` 只覆盖 `JS_GC` 入口**。`misc.rs` 里直接调用 `JS_RunGC` 的
   `NonIncrementalGC` / `StartIncrementalGC` 不计入；当前 Servo 未使用这两条路径。
3. **进程在 30 秒后被 SIGKILL**。页面执行完会空闲、不会自己退出，因此没有走正常关闭路径；
   关闭时的 GC 不在统计范围内。

## 复现

重新执行上面的循环，三行都应为 0，且日志文件长度为 0 字节。
