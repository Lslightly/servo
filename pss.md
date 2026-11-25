# PSS 对比流程

两个脚本都要用 **release** 二进制，两边的构建配置保持一致再比较。

## 1. 采样（每个仓库各跑一次）

```bash
bash pss-rounds.sh <servoshell 二进制> "file://<pss-rounds.html 的绝对路径>" <label> <输出目录>
```

产出 `<输出目录>/<label>.csv`（每轮一行）、`.log`（页面 console）、`.meta`（二进制/URL/时间）。

本仓库（lazy-binding-pure）：

```bash
bash /home/lqw/mygit/servo-ohos/pss-rounds.sh \
  /home/lqw/mygit/servo-ohos/target/release/servoshell \
  "file:///home/lqw/mygit/servo-ohos/pss-rounds.html" \
  lazy /home/lqw/mygit/servo-ohos/.vscode/logs/pss-rounds/lazy
```

base 仓库：

```bash
bash /home/lqw/mygit/servo-ohos/pss-rounds.sh \
  /home/lqw/mygit/servo-lazy-binding-base/servo-ohos-base/target/release/servoshell \
  "file:///home/lqw/mygit/servo-ohos/pss-rounds.html" \
  baseline /home/lqw/mygit/servo-ohos/.vscode/logs/pss-rounds/base
```

页面参数可加在 URL 上：`pss-rounds.html?n=50000&rounds=48&settle=400`。

## 2. 画对比图

```bash
cd /home/lqw/mygit/servo-ohos
.venv/bin/python3 pss-compare.py \
  baseline=.vscode/logs/pss-rounds/base/baseline.csv \
  lazy=.vscode/logs/pss-rounds/lazy/lazy.csv \
  -o pss-compare.png
```

没有装依赖的机器上用：

```bash
uv run --with pandas --with matplotlib python pss-compare.py a=a.csv b=b.csv -o compare.png
```

默认两栏（PSS total、PSS anon），要看全部指标加 `--panels total,anon,file,shmem`。

## 本次结果

- baseline: 186.1 → 268.3 MiB (Δ +82.2)
- lazy: 185.4 → 265.1 MiB (Δ +79.8)
