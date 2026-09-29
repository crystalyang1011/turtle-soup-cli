#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""海龟汤题源 ETL（开发期预拉 + 入库）。

本脚本是**清洗规则的唯一事实源**（见 design-doc/04-题源与ETL.md §7）：
运行时 dataset.rs 复用同一套规则，并共用 etl_rules.json。

用法:
    python etl_turtlebench.py --limit 100 --out ../../assets/puzzles.json
    python etl_turtlebench.py --mirror            # 走 hf-mirror 镜像

依赖: requests（pip install requests）。数据集字段格式尚未核验，解析保持宽容。
by AI.Coding
"""
from __future__ import annotations

import argparse
import hashlib
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

HF_ENDPOINT = (
    "https://datasets-server.huggingface.co/rows"
    "?dataset=Duguce%2FTurtleBench1.5k&config=default&split=train"
)
MIRROR_ENDPOINT = (
    "https://hf-mirror.com/datasets-server/rows"
    "?dataset=Duguce%2FTurtleBench1.5k&config=default&split=train"
)

MIN_FACTS, MAX_FACTS = 4, 6


def is_blocked(text: str) -> bool:
    """命中血腥/色情词则过滤；恐怖/灵异不设过滤词（04 §4）。"""
    return any(k in text for k in BLOCKED_KEYWORDS)


def short_hash(s: str) -> str:
    return hashlib.md5(s.encode("utf-8")).hexdigest()[:8]


def pick(row: dict, keys: list[str]) -> str | None:
    for k in keys:
        v = row.get(k)
        if isinstance(v, str) and v.strip():
            return v.strip()
    return None


def collect_positive(row: dict) -> list[str]:
    out: list[str] = []
    for key in ("positive_guesses", "truths", "t_guesses", "correct_guesses"):
        arr = row.get(key)
        if isinstance(arr, list):
            out += [str(x).strip() for x in arr if str(x).strip()]
    return out


def etl_to_puzzle(row: dict, difficulty: int) -> dict | None:
    row = row.get("row", row)
    surface = pick(row, ["surface", "story", "puzzle", "soup_surface"])
    bottom = pick(row, ["bottom", "truth", "answer", "soup_bottom"])
    if not surface or not bottom:
        return None
    if is_blocked(surface) or is_blocked(bottom):
        return None

    facts: list[str] = []
    for g in collect_positive(row):
        if g not in facts:
            facts.append(g)
    if not (MIN_FACTS <= len(facts) <= MAX_FACTS):
        return None

    key_facts = [{"text": t, "core": i == 0} for i, t in enumerate(facts)]
    return {
        "id": f"ds-{pick(row, ['id', 'sid', 'story_id']) or short_hash(surface)}",
        "title": surface[:12],
        "surface": surface,
        "truth": bottom,
        "key_facts": key_facts,
        "difficulty": max(1, min(5, difficulty)),
        "tags": ["dataset"],
        "source": "dataset",
        "created_at": 0,
    }


def fetch_rows(endpoint: str, offset: int, length: int) -> list[dict]:
    url = f"{endpoint}&offset={offset}&length={length}"
    r = requests.get(url, timeout=30)
    r.raise_for_status()
    return r.json().get("rows", [])


def main() -> int:
    ap = argparse.ArgumentParser(description="海龟汤题源 ETL")
    ap.add_argument("--limit", type=int, default=100, help="拉取条数")
    ap.add_argument("--offset", type=int, default=0, help="起始偏移")
    ap.add_argument("--difficulty", type=int, default=2, help="默认难度")
    ap.add_argument("--out", type=str, default=str(Path(__file__).resolve().parents[2] / "assets" / "puzzles.json"))
    ap.add_argument("--mirror", action="store_true", help="走 hf-mirror 镜像")
    args = ap.parse_args()

    endpoint = MIRROR_ENDPOINT if args.mirror else HF_ENDPOINT
    rows = fetch_rows(endpoint, args.offset, args.limit)
    puzzles = [p for row in rows if (p := etl_to_puzzle(row, args.difficulty))]
    for p in puzzles:
        p["created_at"] = 0

    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    doc = {"schemaVersion": 1, "puzzles": puzzles}
    out.write_text(json.dumps(doc, ensure_ascii=False, indent=2), encoding="utf-8")
    print(f"拉取 {len(rows)} 条 → 入库 {len(puzzles)} 题 → {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
