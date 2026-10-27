/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! 按 **JS 对象**把 mozjs 的引用计数事件记录成 folded stack，用于生成 flame chart。
//!
//! 关键点：祖先不是手写 span，而是 event 触发时的**真实调用栈**。
//!
//! - `on_event` 里只做轻量的事：记时间戳、抓一个未解析的 backtrace；
//! - 事件按 (tag, ptr) 分组，**每个对象一条独立时间线**；
//! - 缓存最多 `SERVO_EVENT_FLAME_OBJECTS`（默认 20）个对象。满了以后：
//!   优先丢掉已经 drop 的对象；没有 drop 的对象时，丢掉**创建时间最晚**的那个，
//!   好让长生命周期对象的事件列表尽量留下来；
//! - `FlushGuard` 在退出时统一解析符号，**每个对象写一个** folded 文件；
//! - 每个事件占**一格**（count 固定为 1），x 轴是事件顺序，不是时间；
//! - 每条时间线的最顶层补一个虚拟帧 `retain` / `release` / `drain`；进入 pool 的事件
//!   （`retain_borrowed_pool` / `retain_root_pool` / `adopt_result_pool`）直接用事件名
//!   当最顶层，以便区分来源。事件名本身通常在真实栈里，所以不再重复 push。
//!
//! 为什么宽度不用时间：按对象分组后，「到下一条事件」的间隔大部分与该对象无关；
//! 而且最后一条会一直算到 flush（活着的对象）或 drop，drain 之类的事件会因此
//! 占满整张图。用事件数更直观。
//!
//! 为什么不在 `on_event` 里直接解析符号：符号解析很慢，延迟到 flush 可以避免拖慢
//! 抓事件的热路径。
//!
//! ```sh
//! SERVO_EVENT_FLAME_DIR=./flame ./servoshell ...
//! # 可选：只保留以某前缀开头的帧；缓存的对象数
//! SERVO_EVENT_FLAME_PREFIX=servo
//! SERVO_EVENT_FLAME_OBJECTS=50
//! # 只记录某一类对象（mozjs 事件的 class 字段，如 HTMLDivElement）
//! SERVO_EVENT_FLAME_CLASS=HTMLDivElement
//! # 每个对象一个文件：./flame/event-flame-obj0001.folded、obj0002.folded...
//! inferno-flamegraph --flamechart ./flame/event-flame-obj0001.folded > obj0001.svg
//! ```
//!
//! `SERVO_TRACING` 里的 `mozjs_gc=trace` 由 `init_tracing` 自动加上。

use std::collections::HashMap;
use std::fmt;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Once};
use std::time::Instant;

use backtrace::Backtrace;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

/// mozjs 发出、本 layer 收集的 tracing target。mozjs 里的事件也用这个字面量。
pub const TARGET: &str = "mozjs_gc";

/// mozjs 在最后一个引用消失时发出的标记事件（同时记入时间线，用于结束该对象）。
const DROPPED: &str = "drop_object_pool";

/// 默认缓存多少个对象。
const DEFAULT_OBJECTS: usize = 20;

/// 对象标识：`(JSValue tag, ptr)`。不同 tag 的 ptr 可能撞号，必须带上 tag。
type Key = (i32, usize);

struct EventRecord {
    /// 事件名，作为火焰图最内层的叶子。
    leaf: String,
    /// 操作类型，作为叶子上面那一层虚拟帧：`retain` / `release` / `drain`。
    kind: String,
    /// 未解析的调用栈；等到 flush 再解析符号。
    backtrace: Backtrace,
}

/// 一个对象的事件时间线。
struct ObjectTrace {
    /// 输出文件名用的序号。
    id: u64,
    /// 第一次看到它的时刻，缓存淘汰时用来比较。
    created: Instant,
    /// 最后一个引用消失的时刻；`Some` 表示可以被优先替换。
    dropped: Option<Instant>,
    events: Vec<EventRecord>,
}

struct State {
    objects: HashMap<Key, ObjectTrace>,
    capacity: usize,
    next_id: u64,
}

impl State {
    fn new(capacity: usize) -> Self {
        Self {
            objects: HashMap::new(),
            capacity,
            next_id: 1,
        }
    }

    /// 记录一个事件；`leaf == DROPPED` 时同时标记对象已释放（也记入时间线）。
    fn record(
        &mut self,
        key: Key,
        at: Instant,
        leaf: &str,
        kind: &str,
        backtrace: Option<Backtrace>,
    ) {
        if !self.objects.contains_key(&key) {
            if self.objects.len() >= self.capacity {
                self.evict();
            }
            let id = self.next_id;
            self.next_id += 1;
            self.objects.insert(
                key,
                ObjectTrace {
                    id,
                    created: at,
                    dropped: None,
                    events: Vec::new(),
                },
            );
        }

        let object = self.objects.get_mut(&key).expect("just inserted");
        if leaf == DROPPED {
            // 已释放：记下时间，供缓存优先淘汰。
            object.dropped = Some(at);
        } else {
            // 还活着（或者 pool 里的最后一个引用尚未 drain）：清掉上一次的释放标记。
            object.dropped = None;
        }
        object.events.push(EventRecord {
            leaf: leaf.to_owned(),
            kind: kind.to_owned(),
            backtrace: backtrace.expect("timeline events carry a backtrace"),
        });
    }

    /// 缓存满了：优先丢已释放的对象；否则丢创建时间最晚的，保住长生命周期对象。
    fn evict(&mut self) {
        let victim = self
            .objects
            .iter()
            .filter(|(_, object)| object.dropped.is_some())
            .min_by_key(|(_, object)| object.created)
            .or_else(|| self.objects.iter().max_by_key(|(_, object)| object.created))
            .map(|(key, _)| *key);
        if let Some(key) = victim {
            self.objects.remove(&key);
        }
    }
}

pub struct EventFlameLayer {
    target: String,
    state: Arc<Mutex<State>>,
    /// 未设置 `SERVO_EVENT_FLAME_DIR` 时为 false：事件直接丢弃，退出时也不写文件。
    enabled: bool,
    /// 只记录类名包含该字符串的对象（`SERVO_EVENT_FLAME_CLASS`）；`None` 表示不过滤。
    ///
    /// 类名来自 mozjs 事件的 `class` 字段（如 `HTMLDivElement`），其余值（字符串、
    /// BigInt、普通对象）是空字符串。不匹配的事件连 backtrace 都不抓。
    class_filter: Option<String>,
}

/// 在 drop 时解析 backtrace，并给每个对象写一个 folded 文件。
pub struct FlushGuard {
    /// 输出目录；文件名为 `event-flame-objNNNN.folded`。
    dir: PathBuf,
    /// 只保留以该前缀开头的栈帧；空字符串表示保留所有帧。
    frame_prefix: String,
    state: Arc<Mutex<State>>,
    enabled: bool,
}

/// 进程级的 guard：保活到进程退出，由 `atexit` 触发写入。
static GUARD: Mutex<Option<FlushGuard>> = Mutex::new(None);

impl EventFlameLayer {
    pub fn with_dir(
        dir: impl Into<PathBuf>,
        target: impl Into<String>,
        frame_prefix: impl Into<String>,
        capacity: usize,
    ) -> (Self, FlushGuard) {
        let state = Arc::new(Mutex::new(State::new(capacity.max(1))));
        let layer = Self {
            target: target.into(),
            state: Arc::clone(&state),
            enabled: true,
            class_filter: None,
        };
        let guard = FlushGuard {
            dir: dir.into(),
            frame_prefix: frame_prefix.into(),
            state,
            enabled: true,
        };
        (layer, guard)
    }

    /// 未启用时的占位 layer：类型不变，便于无条件加进 subscriber。
    pub fn disabled() -> (Self, FlushGuard) {
        let state = Arc::new(Mutex::new(State::new(1)));
        let layer = Self {
            target: String::new(),
            state: Arc::clone(&state),
            enabled: false,
            class_filter: None,
        };
        let guard = FlushGuard {
            dir: PathBuf::new(),
            frame_prefix: String::new(),
            state,
            enabled: false,
        };
        (layer, guard)
    }

    /// 按环境变量创建：
    ///
    /// - `SERVO_EVENT_FLAME_DIR`：输出目录，不存在时会创建；设置为空字符串时用当前目录。
    ///   未设置则返回 `None`（不记录）。
    /// - `SERVO_EVENT_FLAME_PREFIX`：只保留以该前缀开头的栈帧。
    /// - `SERVO_EVENT_FLAME_OBJECTS`：缓存多少个对象，默认 20。
    /// - `SERVO_EVENT_FLAME_CLASS`：只记录类名包含该字符串的对象（如 `HTMLDivElement`）。
    pub fn from_env() -> Option<(Self, FlushGuard)> {
        let dir = std::env::var_os("SERVO_EVENT_FLAME_DIR")?;
        let dir = if dir.is_empty() {
            PathBuf::from(".")
        } else {
            PathBuf::from(dir)
        };
        let prefix = std::env::var("SERVO_EVENT_FLAME_PREFIX").unwrap_or_default();
        let capacity = std::env::var("SERVO_EVENT_FLAME_OBJECTS")
            .ok()
            .and_then(|n| n.parse().ok())
            .unwrap_or(DEFAULT_OBJECTS);
        let class_filter = std::env::var("SERVO_EVENT_FLAME_CLASS")
            .ok()
            .filter(|class| !class.is_empty());
        let (mut layer, guard) = Self::with_dir(dir, TARGET, prefix, capacity);
        layer.class_filter = class_filter;
        Some((layer, guard))
    }
}

/// 保存 guard 到进程退出，然后用 `atexit` 写入 folded 文件。
///
/// 普通 `main` 返回和 `process::exit` 都会跑 `atexit`；SIGKILL / abort 不会。
pub fn install(guard: FlushGuard) {
    *GUARD.lock().expect("event flame guard poisoned") = Some(guard);

    static REGISTERED: Once = Once::new();
    REGISTERED.call_once(|| unsafe {
        libc::atexit(flush_at_exit);
    });
}

extern "C" fn flush_at_exit() {
    flush();
}

/// 立即解析符号并写出 folded 文件；重复调用无副作用。
pub fn flush() {
    let guard = GUARD.lock().expect("event flame guard poisoned").take();
    // `FlushGuard::drop` 写文件。
    drop(guard);
}

impl<S> Layer<S> for EventFlameLayer
where
    S: Subscriber,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        if !self.enabled || event.metadata().target() != self.target.as_str() {
            return;
        }

        let mut fields = EventFields {
            class_filter: self.class_filter.as_deref(),
            ..Default::default()
        };
        event.record(&mut fields);
        let (Some(tag), Some(ptr)) = (fields.tag, fields.ptr) else {
            return;
        };
        // 先按类名过滤，再抓栈：不关心的事件不付抓栈的代价。
        if self.class_filter.is_some() && !fields.class_matches {
            return;
        }
        let at = Instant::now();
        let leaf = event_name(event.metadata().name(), fields.message.as_deref());
        // 虚拟帧的类型：mozjs 显式给了 `kind` 就用它，否则按事件名推导。
        let kind = fields.kind.unwrap_or_else(|| kind_of(&leaf).to_owned());
        let backtrace = Backtrace::new_unresolved();

        self.state
            .lock()
            .expect("event flame lock poisoned")
            .record(
                (tag as i32, ptr as usize),
                at,
                &leaf,
                &kind,
                Some(backtrace),
            );
    }
}

impl Drop for FlushGuard {
    fn drop(&mut self) {
        if !self.enabled {
            return;
        }
        let mut state = self.state.lock().expect("event flame lock poisoned");
        if state.objects.is_empty() {
            eprintln!(
                "event-flame: 没有记录到 {TARGET} 事件，未写 {}",
                self.dir.display()
            );
            return;
        }
        if let Err(err) = std::fs::create_dir_all(&self.dir) {
            eprintln!("event-flame: 无法创建目录 {}: {err}", self.dir.display());
            return;
        }

        let mut written = 0;
        for object in state.objects.values_mut() {
            // 统一解析符号。这一步比较慢，但此时已经不在事件的热路径上。
            for event in &mut object.events {
                event.backtrace.resolve();
            }
            let path = output_path(&self.dir, object.id);
            match write_object(&path, object, &self.frame_prefix) {
                Ok(()) => written += 1,
                Err(err) => eprintln!("写入 {} 失败: {err}", path.display()),
            }
        }
        eprintln!(
            "event-flame: {written} 个对象写入 {}/event-flame-objNNNN.folded",
            self.dir.display()
        );
    }
}

/// `dir` + 序号 -> `dir/event-flame-obj0001.folded`
fn output_path(dir: &Path, id: u64) -> PathBuf {
    dir.join(format!("event-flame-obj{id:04}.folded"))
}

fn write_object(path: &Path, object: &ObjectTrace, frame_prefix: &str) -> std::io::Result<()> {
    let file = File::create(path)?;
    let mut out = BufWriter::new(file);
    let events = &object.events;

    // 按时间**倒序**写：inferno 的 --flamechart 内部会把输入反转一次，
    // 反转后左边最早、右边最晚。
    for i in (0..events.len()).rev() {
        let stack = stack_of(
            &events[i].backtrace,
            frame_prefix,
            &events[i].leaf,
            &events[i].kind,
        );
        // 每个事件一格。间隔时间在按对象分组后参考价值低（见模块文档），
        // 固定 1 可以让每格的宽度只表示「一个事件」。
        writeln!(out, "{stack} 1")?;
    }
    out.flush()
}

/// 从 backtrace 里提取「由外到内」的应用栈帧，并接上事件名和操作类型。
fn stack_of(backtrace: &Backtrace, prefix: &str, leaf: &str, kind: &str) -> String {
    let mut frames = Vec::new();

    // backtrace.frames() 是从当前函数往外：on_event -> ... -> main。
    for frame in backtrace.frames() {
        for symbol in frame.symbols() {
            let Some(name) = symbol.name() else {
                continue;
            };
            // SymbolName 的 Display 已经是 demangle 后的名字。
            let name = normalize(&name.to_string());

            if !prefix.is_empty() && !name.starts_with(prefix) {
                continue;
            }

            let label = frame_label(&name, symbol);

            // 同一个函数可能因内联/递归连续出现，去掉相邻重复。
            if frames.last() != Some(&label) {
                frames.push(label);
            }
        }
    }

    // folded 要求根在前、叶子在后。
    frames.reverse();
    // 去掉 layer 自身、tracing 派发链和宏展开出的闭包：它们在真实调用点上方。
    strip_tracing_frames(&mut frames);
    frames.push(leaf.to_string());
    frames.join("; ")
}

/// 一帧的显示名：`函数名 (文件:行)`；没有行号信息时退回函数名。
fn frame_label(name: &str, symbol: &backtrace::BacktraceSymbol) -> String {
    match (symbol.filename(), symbol.lineno()) {
        (Some(file), Some(line)) => format!("{name} ({}:{line})", file.display()),
        _ => name.to_owned(),
    }
}

/// 去掉栈顶（最内层）连续的 tracing 内部帧。
///
/// 这些帧在真实调用点之上：`trace!` 的展开、`tracing_core` 的派发、
/// `EventFlameLayer::on_event`，以及 backtrace 自己的抓栈函数。
fn strip_tracing_frames(frames: &mut Vec<String>) {
    while let Some(frame) = frames.last() {
        if !is_tracing_frame(frame) {
            break;
        }
        frames.pop();
    }
}

fn is_tracing_frame(name: &str) -> bool {
    // `<servoshell::event_flame::... as tracing_subscriber::layer::Layer<S>>::on_event`
    // 这种名字以 `<` 开头。
    let name = name.trim_start_matches('<');

    [
        "tracing::",
        "tracing_core::",
        "tracing_subscriber::",
        "backtrace::",
    ]
    .iter()
    .any(|prefix| name.starts_with(prefix))
        || name.contains("event_flame")
        || name.contains("{{closure")
}

/// 事件名对应的操作类型，用作最顶层的虚拟帧。
fn kind_of(leaf: &str) -> &'static str {
    match leaf {
        "retain_object" => "retain",
        "push_to_defer_pool" | "release_JS_FreeValue" => "release",
        "JS_FreeValue_object_pool" | DROPPED => "drain",
        // `retain_borrowed_pool` / `retain_root_pool` / `adopt_result_pool` 自带名字，
        // 不再套 kind 帧。
        _ => "",
    }
}

/// 事件名：显式命名的事件用名字；`trace!("...")` 的 metadata 名是
/// `event file:line`，此时退回 message。
fn event_name(meta_name: &str, message: Option<&str>) -> String {
    let leaf = if meta_name.starts_with("event ") {
        message.unwrap_or(meta_name)
    } else {
        meta_name
    };
    clean(leaf)
}

#[derive(Default)]
struct EventFields<'a> {
    message: Option<String>,
    /// `tag = ...` 字段。
    tag: Option<i64>,
    /// `ptr = ...` 字段。
    ptr: Option<u64>,
    /// `kind = "retain" | "release" | "drain"` 字段（目前 mozjs 不传）。
    kind: Option<String>,
    /// 类名过滤条件（`SERVO_EVENT_FLAME_CLASS`）；`None` 时不关心 `class` 字段。
    class_filter: Option<&'a str>,
    /// `class = "HTMLDivElement" | ""` 是否包含 `class_filter`。
    class_matches: bool,
}

impl Visit for EventFields<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "message" => self.message = Some(value.to_string()),
            "kind" => self.kind = Some(value.to_string()),
            // 只比较，不复制：没有过滤条件时什么都不做。
            "class" => {
                self.class_matches = self
                    .class_filter
                    .is_some_and(|filter| value.contains(filter));
            },
            _ => {},
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" && self.message.is_none() {
            self.message = Some(format!("{value:?}"));
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        if field.name() == "tag" {
            self.tag = Some(value);
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        if field.name() == "ptr" {
            self.ptr = Some(value);
        }
    }
}

/// 去掉 demangle 结果里的 crate disambiguator（如 `foo[fd3cd53036836ddd]::bar`）
/// 和旧版 mangling 末尾的 `::h0123456789abcdef`。
fn normalize(name: &str) -> String {
    let name = strip_crate_hash(name);
    clean(strip_hash(&name))
}

fn strip_crate_hash(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut rest = name;

    while let Some(start) = rest.find('[') {
        out.push_str(&rest[..start]);

        let Some(end_rel) = rest[start..].find(']') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let end = start + end_rel;
        let inner = &rest[start + 1..end];

        // 只去掉形如 [16 位十六进制] 的 crate hash，避免误伤泛型参数。
        if inner.len() == 16 && inner.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            rest = &rest[end + 1..];
        } else {
            out.push_str(&rest[start..=end]);
            rest = &rest[end + 1..];
        }
    }

    out.push_str(rest);
    out
}

/// 去掉旧版 Rust mangling 末尾的 `::h0123456789abcdef`。
fn strip_hash(name: &str) -> &str {
    if let Some(pos) = name.rfind("::h") {
        let hash = &name[pos + 3..];
        if hash.len() == 16 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return &name[..pos];
        }
    }
    name
}

/// folded 格式用分号分隔帧，所以帧名里不能出现分号。
fn clean(name: &str) -> String {
    name.replace(';', ":").replace('\n', " ")
}
