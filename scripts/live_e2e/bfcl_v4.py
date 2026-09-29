#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import math
import os
import random
import subprocess
import sys
import time
from dataclasses import replace
from pathlib import Path
from typing import Any

if __package__ in (None, ""):
    sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from live_e2e.common import (  # noqa: E402
    DEFAULT_ARTIFACT_DIR,
    DEFAULT_DSTACK_ENDPOINT,
    DEFAULT_ENV_FILE,
    ROOT,
    Provider,
    find_free_port,
    load_dotenv,
    load_providers,
    merged_env,
    write_json,
)
from live_e2e.launch_aggregator import AggregatorProcess  # noqa: E402
from live_e2e.preflight import preflight  # noqa: E402


DEFAULT_BFCL_MODEL = "openbmb/MiniCPM-SALA-FC"
DEFAULT_BFCL_CATEGORIES = ["single_turn", "multi_turn"]


def main() -> None:
    parser = argparse.ArgumentParser(
        description=(
            "Run a sampled BFCL v4 function-calling evaluation through the local "
            "ACI aggregator."
        )
    )
    parser.add_argument("--providers-file", type=Path, default=ROOT / "scripts/live_e2e/providers.json")
    parser.add_argument("--provider", action="append", default=[])
    parser.add_argument("--env-file", type=Path, default=DEFAULT_ENV_FILE)
    parser.add_argument("--port", type=int, default=18086)
    parser.add_argument("--dstack-endpoint", default=DEFAULT_DSTACK_ENDPOINT)
    parser.add_argument("--artifacts-dir", type=Path, default=DEFAULT_ARTIFACT_DIR)
    parser.add_argument(
        "--bfcl-dir",
        type=Path,
        default=Path(
            os.getenv(
                "BFCL_DIR",
                ROOT.parent / "gorilla-bfcl" / "berkeley-function-call-leaderboard",
            )
        ),
        help="Local gorilla/berkeley-function-call-leaderboard checkout.",
    )
    parser.add_argument(
        "--bfcl-model",
        default=DEFAULT_BFCL_MODEL,
        help=(
            "BFCL-supported model key whose handler uses OpenAI Chat Completions "
            "native tools. The aggregator exposes this alias during the run."
        ),
    )
    parser.add_argument(
        "--test-category",
        action="append",
        default=[],
        help=(
            "BFCL category or category group. Repeat or pass comma-separated values. "
            "Defaults to single_turn,multi_turn."
        ),
    )
    parser.add_argument("--sample-rate", type=float, default=0.01)
    parser.add_argument(
        "--sample-mode",
        choices=["deterministic", "random"],
        default="deterministic",
        help="Select stable spread-by-ID samples by default; use random only when explicitly requested.",
    )
    parser.add_argument("--sample-seed", type=int, default=20260516)
    parser.add_argument(
        "--min-per-category",
        type=int,
        default=2,
        help="Minimum sampled IDs per category. BFCL's leaderboard CSV step needs at least two latency points.",
    )
    parser.add_argument("--max-cases", type=int, default=None)
    parser.add_argument("--num-threads", type=int, default=1)
    parser.add_argument("--timeout-seconds", type=int, default=3600)
    parser.add_argument("--no-build", action="store_true")
    args = parser.parse_args()

    if args.port == 0:
        args.port = find_free_port()
    artifact_dir = args.artifacts_dir / time.strftime("%Y%m%d-%H%M%S-bfcl-v4")
    artifact_dir.mkdir(parents=True, exist_ok=True)

    load_dotenv(args.env_file)
    providers = load_providers(args.providers_file, args.provider)
    bfcl_dir = resolve_bfcl_dir(args.bfcl_dir)
    requested_categories = normalize_categories(args.test_category) or DEFAULT_BFCL_CATEGORIES
    sample = build_sample(
        bfcl_dir=bfcl_dir,
        requested_categories=requested_categories,
        sample_rate=args.sample_rate,
        sample_mode=args.sample_mode,
        sample_seed=args.sample_seed,
        min_per_category=args.min_per_category,
        max_cases=args.max_cases,
    )

    summary: dict[str, Any] = {
        "artifact_dir": str(artifact_dir),
        "bfcl_dir": str(bfcl_dir),
        "bfcl_model": args.bfcl_model,
        "requested_categories": requested_categories,
        "sample_rate": args.sample_rate,
        "sample_mode": args.sample_mode,
        "sample_seed": args.sample_seed,
        "sample": sample["summary"],
        "providers": [provider.name for provider in providers],
        "results": [],
    }

    try:
        summary["preflight"] = preflight(
            providers,
            port=args.port,
            dstack_endpoint=args.dstack_endpoint,
            env_file=args.env_file,
            check_build=not args.no_build,
        )
        for provider in providers:
            provider_result = run_provider_bfcl(
                provider=provider,
                bfcl_public_model=args.bfcl_model,
                bfcl_dir=bfcl_dir,
                sampled_ids=sample["ids"],
                sampled_categories=sample["categories"],
                port=args.port,
                dstack_endpoint=args.dstack_endpoint,
                artifact_dir=artifact_dir / provider.name,
                timeout_seconds=args.timeout_seconds,
                num_threads=args.num_threads,
            )
            summary["results"].append(provider_result)

        summary["ok"] = all(result["ok"] for result in summary["results"])
        write_json(artifact_dir / "summary.json", summary)
        print(json.dumps(summary, indent=2, sort_keys=True))
        if not summary["ok"]:
            raise SystemExit(1)
    except Exception as exc:  # noqa: BLE001 - preserve artifact summary on failures.
        summary["ok"] = False
        summary["error"] = str(exc)
        write_json(artifact_dir / "summary.json", summary)
        print(json.dumps(summary, indent=2, sort_keys=True), file=sys.stderr)
        raise


def normalize_categories(values: list[str]) -> list[str]:
    categories: list[str] = []
    for value in values:
        categories.extend(part.strip() for part in value.split(",") if part.strip())
    return categories


def resolve_bfcl_dir(path: Path) -> Path:
    path = path.expanduser().resolve()
    if not (path / "bfcl_eval" / "__main__.py").exists():
        raise FileNotFoundError(
            f"BFCL checkout not found at {path}. Clone "
            "https://github.com/ShishirPatil/gorilla and pass "
            "--bfcl-dir gorilla/berkeley-function-call-leaderboard."
        )
    return path


def import_bfcl_helpers(bfcl_dir: Path) -> tuple[Any, Any]:
    sys.path.insert(0, str(bfcl_dir))
    try:
        from bfcl_eval.utils import (  # type: ignore
            load_dataset_entry,
            parse_test_category_argument,
        )
    except Exception as exc:  # noqa: BLE001 - report dependency setup clearly.
        raise RuntimeError(
            "Could not import BFCL helpers. Run this script with BFCL installed, "
            f"for example: uv run --with-editable {bfcl_dir} --with soundfile "
            "python scripts/live_e2e/bfcl_v4.py ..."
        ) from exc
    return parse_test_category_argument, load_dataset_entry


def build_sample(
    *,
    bfcl_dir: Path,
    requested_categories: list[str],
    sample_rate: float,
    sample_mode: str,
    sample_seed: int,
    min_per_category: int,
    max_cases: int | None,
) -> dict[str, Any]:
    if sample_rate <= 0:
        raise ValueError("--sample-rate must be positive")
    if min_per_category < 0:
        raise ValueError("--min-per-category must be non-negative")
    if max_cases is not None and max_cases < 2:
        raise ValueError("--max-cases must be at least 2 for BFCL evaluation")
    parse_test_category_argument, load_dataset_entry = import_bfcl_helpers(bfcl_dir)
    categories = parse_test_category_argument(requested_categories)
    rng = random.Random(sample_seed)

    sampled_ids: dict[str, list[str]] = {}
    category_summary: dict[str, dict[str, int]] = {}
    for category in categories:
        entries = load_dataset_entry(category)
        ids = sorted({entry["id"] for entry in entries})
        target = max(min_per_category, math.ceil(len(ids) * sample_rate))
        target = min(target, len(ids))
        selected = select_ids(ids, target, sample_mode=sample_mode, rng=rng)
        sampled_ids[category] = selected
        category_summary[category] = {"available": len(ids), "sampled": len(selected)}

    if max_cases is not None and max_cases < total_sampled(sampled_ids):
        sampled_ids = cap_sampled_ids(
            sampled_ids,
            max_cases=max_cases,
            min_per_category=min_per_category,
            sample_mode=sample_mode,
            rng=rng,
        )
        category_summary = {
            category: {
                "available": category_summary[category]["available"],
                "sampled": len(ids),
            }
            for category, ids in sampled_ids.items()
        }

    return {
        "categories": list(sampled_ids.keys()),
        "ids": sampled_ids,
        "summary": {
            "total_available": sum(item["available"] for item in category_summary.values()),
            "total_sampled": sum(item["sampled"] for item in category_summary.values()),
            "categories": category_summary,
        },
    }


def total_sampled(sampled_ids: dict[str, list[str]]) -> int:
    return sum(len(ids) for ids in sampled_ids.values())


def cap_sampled_ids(
    sampled_ids: dict[str, list[str]],
    *,
    max_cases: int,
    min_per_category: int,
    sample_mode: str,
    rng: random.Random,
) -> dict[str, list[str]]:
    categories = [category for category, ids in sampled_ids.items() if ids]
    if not categories:
        return {}

    category_floor = max(1, min_per_category)
    if max_cases < len(categories) * category_floor:
        category_count = max(1, max_cases // category_floor)
        categories = select_ordered_items(
            categories,
            category_count,
            sample_mode=sample_mode,
            rng=rng,
        )

    capped_counts = {
        category: min(category_floor, len(sampled_ids[category]))
        for category in categories
    }
    remaining = max_cases - sum(capped_counts.values())
    while remaining > 0:
        progressed = False
        for category in categories:
            if remaining <= 0:
                break
            if capped_counts[category] < len(sampled_ids[category]):
                capped_counts[category] += 1
                remaining -= 1
                progressed = True
        if not progressed:
            break

    return {
        category: select_ids(
            sampled_ids[category],
            capped_counts[category],
            sample_mode=sample_mode,
            rng=rng,
        )
        for category in categories
        if capped_counts[category] > 0
    }


def select_ordered_items(
    items: list[str],
    target: int,
    *,
    sample_mode: str,
    rng: random.Random,
) -> list[str]:
    if target <= 0:
        return []
    if target >= len(items):
        return list(items)
    if sample_mode == "random":
        selected = set(rng.sample(range(len(items)), target))
        return [item for index, item in enumerate(items) if index in selected]
    if sample_mode != "deterministic":
        raise ValueError(f"unknown sample mode: {sample_mode}")
    if target == 1:
        return [items[len(items) // 2]]
    indexes = {
        round(index * (len(items) - 1) / (target - 1))
        for index in range(target)
    }
    selected_indexes = sorted(indexes)
    if len(selected_indexes) != target:
        for index in range(len(items)):
            if len(selected_indexes) == target:
                break
            if index not in indexes:
                selected_indexes.append(index)
        selected_indexes.sort()
    return [items[index] for index in selected_indexes]


def select_ids(
    ids: list[str],
    target: int,
    *,
    sample_mode: str,
    rng: random.Random,
) -> list[str]:
    if target <= 0:
        return []
    if target >= len(ids):
        return list(ids)
    if sample_mode == "random":
        return sorted(rng.sample(ids, target))
    if sample_mode != "deterministic":
        raise ValueError(f"unknown sample mode: {sample_mode}")
    if target == 1:
        return [ids[len(ids) // 2]]
    indexes = {
        round(index * (len(ids) - 1) / (target - 1))
        for index in range(target)
    }
    selected = [ids[index] for index in sorted(indexes)]
    if len(selected) != target:
        for item in ids:
            if len(selected) == target:
                break
            if item not in selected:
                selected.append(item)
        selected.sort()
    return selected


def run_provider_bfcl(
    *,
    provider: Provider,
    bfcl_public_model: str,
    bfcl_dir: Path,
    sampled_ids: dict[str, list[str]],
    sampled_categories: list[str],
    port: int,
    dstack_endpoint: str,
    artifact_dir: Path,
    timeout_seconds: int,
    num_threads: int,
) -> dict[str, Any]:
    artifact_dir.mkdir(parents=True, exist_ok=True)
    bfcl_project = artifact_dir / "bfcl-project"
    bfcl_project.mkdir(parents=True, exist_ok=True)
    write_json(bfcl_project / "test_case_ids_to_generate.json", sampled_ids)

    bfcl_provider = replace(provider, public_model=bfcl_public_model)
    with AggregatorProcess(
        [bfcl_provider],
        port=port,
        dstack_endpoint=dstack_endpoint,
        env=merged_env(),
        artifact_dir=artifact_dir,
    ) as aggregator:
        (bfcl_project / ".env").write_text(
            "\n".join(
                [
                    f"OPENAI_BASE_URL={aggregator.base_url.rstrip('/')}/v1",
                    "OPENAI_API_KEY=aci-local",
                    "",
                ]
            ),
            encoding="utf-8",
        )
        env = {
            **os.environ,
            "BFCL_PROJECT_ROOT": str(bfcl_project),
            "OPENAI_BASE_URL": f"{aggregator.base_url.rstrip('/')}/v1",
            "OPENAI_API_KEY": "aci-local",
            "TOKENIZERS_PARALLELISM": "false",
        }
        generate = run_bfcl(
            bfcl_dir,
            [
                "generate",
                "--model",
                bfcl_public_model,
                "--run-ids",
                "--allow-overwrite",
                "--num-threads",
                str(num_threads),
                "--result-dir",
                "result",
            ],
            cwd=bfcl_dir,
            env=env,
            stdout_path=artifact_dir / "bfcl.generate.stdout",
            stderr_path=artifact_dir / "bfcl.generate.stderr",
            timeout_seconds=timeout_seconds,
        )
        evaluate = run_bfcl(
            bfcl_dir,
            [
                "evaluate",
                "--model",
                bfcl_public_model,
                "--test-category",
                ",".join(sampled_categories),
                "--partial-eval",
                "--result-dir",
                "result",
                "--score-dir",
                "score",
            ],
            cwd=bfcl_dir,
            env=env,
            stdout_path=artifact_dir / "bfcl.evaluate.stdout",
            stderr_path=artifact_dir / "bfcl.evaluate.stderr",
            timeout_seconds=timeout_seconds,
        )

    score = collect_scores(bfcl_project, bfcl_public_model)
    result = {
        "provider": provider.name,
        "public_model": provider.public_model,
        "bfcl_public_model": bfcl_public_model,
        "artifact_dir": str(artifact_dir),
        "generate": generate,
        "evaluate": evaluate,
        "score": score,
        "ok": generate["returncode"] == 0
        and evaluate["returncode"] == 0
        and score["total_scored"] > 0,
    }
    write_json(artifact_dir / "summary.json", result)
    return result


def run_bfcl(
    bfcl_dir: Path,
    args: list[str],
    *,
    cwd: Path,
    env: dict[str, str],
    stdout_path: Path,
    stderr_path: Path,
    timeout_seconds: int,
) -> dict[str, Any]:
    cmd = [
        "uv",
        "run",
        "--with-editable",
        str(bfcl_dir),
        "--with",
        "soundfile",
        "bfcl",
        *args,
    ]
    completed = subprocess.run(
        cmd,
        cwd=cwd,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        check=False,
        timeout=timeout_seconds,
    )
    stdout_path.write_text(completed.stdout, encoding="utf-8")
    stderr_path.write_text(completed.stderr, encoding="utf-8")
    result = {
        "cmd": cmd,
        "returncode": completed.returncode,
        "stdout_path": str(stdout_path),
        "stderr_path": str(stderr_path),
    }
    if completed.returncode != 0:
        result["stderr_tail"] = completed.stderr[-4000:]
        raise RuntimeError(
            f"BFCL command failed with status {completed.returncode}: {' '.join(cmd)}\n"
            f"stderr: {completed.stderr[-1200:]}"
        )
    return result


def collect_scores(project_root: Path, bfcl_model: str) -> dict[str, Any]:
    model_dir = bfcl_model.replace("/", "_")
    score_root = project_root / "score" / model_dir
    result_root = project_root / "result" / model_dir
    categories: dict[str, Any] = {}
    total_scored = 0
    total_valid = 0
    for score_file in sorted(score_root.rglob("*_score.json")):
        data = load_score_file(score_file)
        if isinstance(data, list):
            scored = len(data)
            valid = sum(1 for item in data if isinstance(item, dict) and item.get("valid") is True)
        elif isinstance(data, dict):
            if "total_count" in data and "correct_count" in data:
                scored = int(data["total_count"])
                valid = int(data["correct_count"])
            else:
                entries = data.get("entries") if isinstance(data.get("entries"), list) else []
                scored = len(entries)
                valid = sum(
                    1
                    for item in entries
                    if isinstance(item, dict) and item.get("valid") is True
                )
        else:
            scored = 0
            valid = 0
        category = score_file.stem.removeprefix("BFCL_v4_").removesuffix("_score")
        categories[category] = {
            "score_file": str(score_file),
            "scored": scored,
            "valid": valid,
            "accuracy": valid / scored if scored else None,
        }
        total_scored += scored
        total_valid += valid

    result_files = sorted(str(path) for path in result_root.rglob("*_result.json"))
    return {
        "score_root": str(score_root),
        "result_root": str(result_root),
        "result_files": result_files,
        "total_scored": total_scored,
        "total_valid": total_valid,
        "accuracy": total_valid / total_scored if total_scored else None,
        "categories": categories,
    }


def load_score_file(score_file: Path) -> Any:
    text = score_file.read_text(encoding="utf-8")
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        for line in text.splitlines():
            if line.strip():
                return json.loads(line)
        raise


if __name__ == "__main__":
    main()
