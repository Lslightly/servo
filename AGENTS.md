# Information for agents

This is a downstream fork of the servo project, which allows AI assisted contributions.
Read @CONTRIBUTING.md before helping the user generate any code.


### Servoshell

The Servoshell crate shouldn't depend on private servo crates. The public API is in the `servo` crate, and if some functionality
from another internal servo crate is needed, it should be re-exported via `servo` instead of directly accessing it from servoshell.
Items can be marked with `#[doc(hidden)]` if the usage is only meant for development purposes in servoshell, and shouldn't be part
of the general embedder API.

### Formatting

Format by running `./mach fmt`.

### Lints

Run `./mach test-tidy` to run lints. 

### Testing

Run unit and integration tests with `./mach test-unit -p <package>` and wpt tests with `./mach test-wpt [--release|--profile=<cargo_profile>] [test-subset].
If you need more control over running integration tests, please always use `cargo nextest` as the test runner, never `cargo test`.
With `cargo nextest` please note that `--profile` refers to the nextest profile and you need to use `--cargo-profile=[dev|medium|release|production|profiling]` to specify the cargo build profile.

### Style guide

The style guide is available in the servo book at <https://book.servo.org/contributing/style-guide.html>.

### MozJS / GC 计数

本 fork 的 `servo-quickjs` 用引用计数 + autorelease pool 管理 JS 对象。
**只有 4 个接口能进 pool：`adopt`、`retain`、`retain_stored`、`release_object`。**
判断某段代码会不会改 JS 计数时，先查这份清单，不要凭函数名猜：

- @../servo-quickjs-GC-pool接口调用点.md —— 4 个接口的语义 + 调用它们的 API 完整清单
- @../servo-quickjs-GC.md —— GC 整体流程

## Web Standards

Servo is a web-rendering engine, and thus should adhere to the web specifications. There is not a single file, but many documents.
When implementing features, or analyzing existing features, we should always compare against the web standard documents.
Deviations should be an exception, well-documented and only occur when other browsers also deviate from the spec.

* CSS related specification documents can be found at <https://drafts.csswg.org/>.