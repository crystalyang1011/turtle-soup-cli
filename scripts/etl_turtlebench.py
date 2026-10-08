#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""海龟汤题源 ETL（开发期预拉 + 入库）。

本脚本是**清洗规则的唯一事实源**（见 design-doc/04-题源与ETL.md §7）：
运行时 dataset.rs 复用同一套规则，并共用 etl_rules.json。

数据形态：中文集为单个 JSONL，每行一条「猜测-标注对」
（id/title/surface/bottom/user_guess/label）。先按 surface+bottom 分组，
再取组内 label=="T" 的 user_guess 作为 key_facts 候选。

用法:
    python etl_turtlebench.py --limit 100 --out ../../assets/puzzles.json
    python etl_turtlebench.py --mirror            # 走 hf-mirror 镜像

依赖: requests（pip install requests）。
by AI.Coding
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

try:
    import requests
except ImportError:  # pragma: no cover
    print("需要 requests: pip install requests", file=sys.stderr)
    raise

# 与 core/src/dataset.rs::BLOCKED_KEYWORDS 保持一致（只过滤血腥/色情，不过滤恐怖/灵异）。
BLOCKED_KEYWORDS = [
    "血腥", "血泊", "肢解", "断肢", "残肢", "内脏", "割喉", "自残", "奸杀", "碎尸",
    "色情", "裸体", "性交", "强奸", "淫", "肉欲",
]

# 直链下载（中文集 JSONL）。rows API 国内不可达、镜像 401，故弃用（见 04 §5.5）。
HF_ENDPOINT = (
    "https://huggingface.co/datasets/Duguce/TurtleBench1.5k"
    "/resolve/main/chinese/zh_data-00000-of-00001.jsonl"
)
MIRROR_ENDPOINT = (
    "https://hf-mirror.com/datasets/Duguce/TurtleBench1.5k"
    "/resolve/main/chinese/zh_data-00000-of-00001.jsonl"
)

MIN_FACTS, MAX_FACTS = 4, 6


def is_blocked(text: str) -> bool:
    """命中血腥/色情词则过滤；恐怖/灵异不设过滤词（04 §4）。"""
    return any(k in text for k in BLOCKED_KEYWORDS)


def short_hash(s: str) -> str:
    """稳定哈希，与 Rust dataset.rs::short_hash **逐字节一致**（含偏移基常量）。

    注意：此处偏移基为项目约定的 1469598103934665603（非标准 FNV-1a 64 基），
    两端必须保持一致，否则同一故事的 id 会不同、破坏 §7 一致性与幂等。
    """
    h = 1469598103934665603
    for b in s.encode("utf-8"):
        h ^= b
        h = (h * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return f"{h & 0xFFFFFFFF:08x}"


def pick(row: dict, keys: list[str]) -> str | None:
    for k in keys:
        v = row.get(k)
        if isinstance(v, str) and v.strip():
            return v.strip()
    return None


def is_correct(label: str | None) -> bool:
    return (label or "").strip().upper() in ("T", "TRUE", "YES", "正确")


def parse_jsonl(text: str) -> tuple[list[dict], int]:
    """逐行解析 JSONL，返回 (有效记录, 非法行数)。"""
    rows: list[dict] = []
    bad = 0
    for line in text.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            obj = json.loads(line)
        except json.JSONDecodeError:
            bad += 1
            continue
        row = obj.get("row", obj) if isinstance(obj, dict) else obj
        if isinstance(row, dict) and pick(row, ["surface", "story", "puzzle", "soup_surface"]) \
                and pick(row, ["bottom", "truth", "answer", "soup_bottom"]):
            rows.append(row)
        else:
            bad += 1
    return rows, bad


def group_stories(rows: list[dict]) -> list[dict]:
    """按 surface+bottom 分组，收集组内 label=="T" 的 user_guess（去重保序）。"""
    index: dict[tuple[str, str], dict] = {}
    order: list[dict] = []
    for row in rows:
        surface = pick(row, ["surface", "story", "puzzle", "soup_surface"])
        bottom = pick(row, ["bottom", "truth", "answer", "soup_bottom"])
        if surface is None or bottom is None:
            continue
        key = (surface, bottom)
        story = index.get(key)
        if story is None:
            story = {
                "title": pick(row, ["title", "name"]) or "",
                "surface": surface,
                "bottom": bottom,
                "positive_guesses": [],
            }
            index[key] = story
            order.append(story)
        guess = pick(row, ["user_guess", "guess", "question"])
        if is_correct(row.get("label")) and guess and guess not in story["positive_guesses"]:
            story["positive_guesses"].append(guess)
    return order


def etl_to_puzzle(story: dict, difficulty: int) -> dict | None:
    surface, bottom = story["surface"], story["bottom"]
    if is_blocked(surface) or is_blocked(bottom):
        return None

    facts: list[str] = []
    for g in story["positive_guesses"]:
        g = g.strip()
        if g and g not in facts:
            facts.append(g)
    if len(facts) < MIN_FACTS:
        return None
    facts = facts[:MAX_FACTS]

    key_facts = [{"text": t, "core": i == 0} for i, t in enumerate(facts)]
    title = story["title"].strip() or surface[:12]
    return {
        "id": f"ds-{short_hash(surface)}",
        "title": title,
        "surface": surface,
        "truth": bottom,
        "key_facts": key_facts,
        "difficulty": max(1, min(5, difficulty)),
        "tags": ["dataset"],
        "source": "dataset",
        "created_at": 0,
    }


def fetch_text(endpoint: str) -> str:
    r = requests.get(endpoint, timeout=60)
    r.raise_for_status()
    # 显式按 UTF-8 解码：HTTP 头未必声明 charset，用 r.text 会按 ISO-8859-1 误解码，
    # 导致 surface 字节变化、与 Rust 侧 hash 不一致（见 04 §7）。
    return r.content.decode("utf-8")


def main() -> int:
    ap = argparse.ArgumentParser(description="海龟汤题源 ETL")
    ap.add_argument("--limit", type=int, default=0, help="最多处理多少个故事（0=全部）")
    ap.add_argument("--difficulty", type=int, default=2, help="默认难度")
    ap.add_argument("--out", type=str, default=str(Path(__file__).resolve().parents[2] / "assets" / "puzzles.json"))
    ap.add_argument("--mirror", action="store_true", help="走 hf-mirror 镜像")
    args = ap.parse_args()

    endpoint = MIRROR_ENDPOINT if args.mirror else HF_ENDPOINT
    text = fetch_text(endpoint)
    rows, bad = parse_jsonl(text)
    if not rows:
        print("远程数据集为空或格式不符（预期 JSONL）", file=sys.stderr)
        return 1
    if bad:
        print(f"跳过 {bad} 行非法 JSONL", file=sys.stderr)

    stories = group_stories(rows)
    if args.limit > 0:
        stories = stories[: args.limit]
    puzzles = [p for story in stories if (p := etl_to_puzzle(story, args.difficulty))]

    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    doc = {"schemaVersion": 1, "puzzles": puzzles}
    out.write_text(json.dumps(doc, ensure_ascii=False, indent=2), encoding="utf-8")
    print(f"题源 {len(stories)} 个故事（共 {len(rows)} 行）→ 入库 {len(puzzles)} 题 → {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
