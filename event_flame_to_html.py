#!/usr/bin/env python3
"""把目录下的 *.folded 各转成一个 SVG，再拼成一个引用这些 SVG 的 HTML。

配合 `SERVO_EVENT_FLAME_DIR`：每个对象一个 `event-flame-objNNNN.folded`，
这里每个文件生成一张 flame chart（同名 .svg），HTML 用 <object> 引用，
不内联 SVG。

用 <object> 而不是 <img>：<img> 不会执行 SVG 里的脚本，缩放/搜索/hover
这些交互就都没了；<object> 会执行。

每次运行会先清掉上次的产物（目录下的 *.svg 和输出 HTML），**清空前必须
人工确认**；`--yes` 跳过确认（脚本/CI 用），`--keep` 不清空。

用法:
    ./event_flame_to_html.py ./flame
    ./event_flame_to_html.py ./flame -o flame.html
    ./event_flame_to_html.py ./flame --tool ~/.cargo/bin/inferno-flamegraph
    ./event_flame_to_html.py ./flame --yes

只用标准库。
"""

import argparse
import html
import os
import re
import subprocess
import sys
from pathlib import Path


def render(folded: Path, tool: str) -> str:
    """跑一次 inferno-flamegraph，返回 SVG 文本。"""
    result = subprocess.run(
        [tool, "--flamechart", str(folded), '--colors', 'rust'],
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        message = result.stderr.strip() or f"exit {result.returncode}"
        raise RuntimeError(message)
    if "<svg" not in result.stdout:
        raise RuntimeError("inferno 的输出里没有 <svg>")
    return result.stdout


STYLE = """
body { margin: 2em; font-family: sans-serif; background: #fff; color: #222; }
h1 { font-size: 1.4em; }
details { margin: 1.5em 0; border-top: 1px solid #ddd; padding-top: 1em; }
summary { cursor: pointer; font-family: monospace; font-size: 1em; }
summary .size { color: #888; margin-left: .5em; }
.chart { margin-top: .5em; }
/* inferno 的 SVG 带 viewBox，配合 aspect-ratio 就能按容器宽度等比缩放。 */
.chart object { display: block; width: 100%; height: auto; }
"""

_WIDTH = re.compile(r'<svg[^>]*\bwidth="([0-9.]+)"')
_HEIGHT = re.compile(r'<svg[^>]*\bheight="([0-9.]+)"')


def svg_size(svg: str) -> tuple[str, str]:
    """从 <svg width=... height=...> 里取尺寸，用来算宽高比。"""
    width = _WIDTH.search(svg)
    height = _HEIGHT.search(svg)
    return (width.group(1) if width else "", height.group(1) if height else "")


def object_style(width: str, height: str) -> str:
    """让 <object> 按容器宽度缩放，高度由 SVG 的宽高比决定。"""
    style = "width:100%;height:auto"
    try:
        if float(width) > 0 and float(height) > 0:
            style += f";aspect-ratio:{width}/{height}"
    except ValueError:
        pass
    return style


def relative_url(path: Path, start: Path) -> str:
    """HTML 里的相对路径；统一用 `/`。"""
    return Path(os.path.relpath(path, start)).as_posix()


def existing_outputs(directory: Path, output: Path) -> list[Path]:
    """上次运行留下的产物：目录下的 *.svg 和输出 HTML。

    只清这些；*.folded 是输入，不动。
    """
    victims = [p for p in sorted(directory.glob("*.svg")) if p.is_file()]
    if output.is_file() and output not in victims:
        victims.append(output)
    return victims


def confirm(question: str, assume_yes: bool) -> bool:
    """删文件前问一句；非交互终端且没有 --yes 时拒绝，不静默删除。"""
    if assume_yes:
        return True
    if not sys.stdin.isatty():
        print(
            "需要确认才能清空旧产物，但当前不是交互终端；确认无误请加 --yes。",
            file=sys.stderr,
        )
        return False
    answer = input(f"{question} [y/N] ").strip().lower()
    return answer in ("y", "yes")


def build_html(
    dir_: Path,
    entries: list[tuple[Path, Path, str, str]],
    output_dir: Path,
    title: str,
) -> str:
    parts = [
        "<!doctype html>",
        '<meta charset="utf-8">',
        f"<title>{html.escape(title)}</title>",
        f"<style>{STYLE}</style>",
        f"<h1>{html.escape(title)}</h1>",
        f"<p>{len(entries)} 个文件，目录 <code>{html.escape(str(dir_))}</code></p>",
    ]
    for folded, svg_path, width, height in entries:
        name = html.escape(folded.name)
        src = html.escape(relative_url(svg_path, output_dir))
        parts.append("<details open>")
        parts.append(
            f'<summary>{name}<span class="size">{folded.stat().st_size} B</span></summary>'
        )
        parts.append('<div class="chart">')
        parts.append(
            f'<object type="image/svg+xml" data="{src}" '
            f'style="{object_style(width, height)}">'
            f"{name}</object>"
        )
        parts.append("</div>")
        parts.append("</details>")
    return "\n".join(parts) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path, help="包含 *.folded 的目录")
    parser.add_argument(
        "-o",
        "--output",
        type=Path,
        help="输出 HTML；默认是 <directory>/flame.html",
    )
    parser.add_argument(
        "--pattern",
        default="*.folded",
        help="匹配的文件名模式（默认 *.folded）",
    )
    parser.add_argument(
        "--tool",
        default="inferno-flamegraph",
        help="inferno-flamegraph 可执行文件（默认从 PATH 找）",
    )
    parser.add_argument("--title", default="Event Flame", help="HTML 标题")
    parser.add_argument(
        "--yes",
        "-y",
        action="store_true",
        help="清空旧产物时不询问（默认必须人工确认）",
    )
    parser.add_argument(
        "--keep",
        action="store_true",
        help="不清空旧产物，直接覆盖同名文件",
    )
    args = parser.parse_args()

    files = sorted(args.directory.glob(args.pattern))
    if not files:
        print(f"{args.directory} 里没有 {args.pattern}", file=sys.stderr)
        return 1

    output = args.output or args.directory / "flame.html"

    if not args.keep:
        victims = existing_outputs(args.directory, output)
        if victims:
            shown = "\n".join(f"  {p}" for p in victims[:10])
            more = f"\n  …另有 {len(victims) - 10} 个" if len(victims) > 10 else ""
            question = (
                f"将删除 {args.directory} 下的 {len(victims)} 个旧产物"
                f"（*.svg 和输出 HTML）：\n{shown}{more}\n继续？"
            )
            if not confirm(question, args.yes):
                print("已取消，没有删除任何文件。", file=sys.stderr)
                return 1
            for victim in victims:
                victim.unlink()

    entries: list[tuple[Path, Path, str, str]] = []
    for folded in files:
        svg_path = folded.with_suffix(".svg").resolve()
        try:
            svg = render(folded, args.tool)
            svg_path.write_text(svg, encoding="utf-8")
            width, height = svg_size(svg)
            entries.append((folded, svg_path, width, height))
        except (RuntimeError, OSError) as err:
            print(f"跳过 {folded}: {err}", file=sys.stderr)

    if not entries:
        print("没有成功转换的文件", file=sys.stderr)
        return 1

    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(
        build_html(args.directory, entries, output.parent.resolve(), args.title),
        encoding="utf-8",
    )
    print(f"{len(entries)}/{len(files)} 个文件写入 {output}（SVG 同名放在 folded 旁边）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
