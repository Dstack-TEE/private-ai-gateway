#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import sys
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import requests

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
    write_json,
)
from live_e2e.launch_aggregator import AggregatorProcess  # noqa: E402
from live_e2e.preflight import preflight  # noqa: E402


@dataclass(frozen=True)
class Stage:
    count: int
    interval_seconds: float

    @classmethod
    def parse(cls, value: str) -> "Stage":
        if "@" not in value:
            raise argparse.ArgumentTypeError("stage must look like COUNT@INTERVAL_SECONDS")
        count_raw, interval_raw = value.split("@", 1)
        try:
            count = int(count_raw)
            interval = float(interval_raw)
        except ValueError as exc:
            raise argparse.ArgumentTypeError(str(exc)) from exc
        if count <= 0:
            raise argparse.ArgumentTypeError("stage count must be positive")
        if interval < 0:
            raise argparse.ArgumentTypeError("stage interval must be non-negative")
        return cls(count=count, interval_seconds=interval)

    def label(self) -> str:
        if self.interval_seconds == 0:
            return f"{self.count}@burst"
        rpm = 60.0 / self.interval_seconds
        return f"{self.count}@{self.interval_seconds:g}s/{rpm:.3g}rpm"


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Probe Chutes completion stability through the ACI aggregator at fixed rates."
    )
    parser.add_argument(
        "--providers-file",
        type=Path,
        default=ROOT / "scripts/live_e2e/providers.glm51.json",
    )
    parser.add_argument("--provider", default="chutes")
    parser.add_argument("--env-file", type=Path, default=DEFAULT_ENV_FILE)
    parser.add_argument("--port", type=int, default=18087)
    parser.add_argument("--dstack-endpoint", default=DEFAULT_DSTACK_ENDPOINT)
    parser.add_argument("--artifacts-dir", type=Path, default=DEFAULT_ARTIFACT_DIR)
    parser.add_argument("--stage", action="append", type=Stage.parse, default=[])
    parser.add_argument("--warmup", type=int, default=1)
    parser.add_argument("--timeout-seconds", type=int, default=900)
    parser.add_argument("--burst-concurrency", type=int, default=8)
    parser.add_argument("--keep-going-after-429", action="store_true")
    parser.add_argument("--no-build", action="store_true")
    args = parser.parse_args()
    if args.burst_concurrency <= 0:
        raise SystemExit("--burst-concurrency must be positive")

    if args.port == 0:
        args.port = find_free_port()
    stages = args.stage or [Stage(3, 60.0), Stage(3, 30.0), Stage(3, 15.0)]
    artifact_dir = args.artifacts_dir / time.strftime("%Y%m%d-%H%M%S-chutes-rate-probe")
    artifact_dir.mkdir(parents=True, exist_ok=True)

    load_dotenv(args.env_file)
    providers = load_providers(args.providers_file, [args.provider])
    if len(providers) != 1:
        raise SystemExit(f"provider selector {args.provider!r} matched {len(providers)} providers")
    provider = providers[0]
    if provider.provider != "chutes":
        raise SystemExit(f"selected provider is {provider.provider!r}, not chutes")

    summary: dict[str, Any] = {
        "artifact_dir": str(artifact_dir),
        "provider": provider.name,
        "public_model": provider.public_model,
        "upstream_model": provider.upstream_model,
        "stages": [stage.label() for stage in stages],
        "warmup": args.warmup,
        "burst_concurrency": args.burst_concurrency,
        "results": [],
    }

    try:
        summary["preflight"] = preflight(
            [provider],
            port=args.port,
            dstack_endpoint=args.dstack_endpoint,
            env_file=args.env_file,
            check_build=not args.no_build,
        )
        with AggregatorProcess(
            [provider],
            port=args.port,
            dstack_endpoint=args.dstack_endpoint,
            artifact_dir=artifact_dir,
        ) as agg:
            sequence = 0
            for index in range(args.warmup):
                sequence += 1
                result = send_probe_request(
                    agg.base_url,
                    provider.public_model,
                    sequence=sequence,
                    stage="warmup",
                    timeout_seconds=args.timeout_seconds,
                )
                summary["results"].append(result)
                write_json(artifact_dir / "summary.json", summary)
                print(json.dumps(result, sort_keys=True), flush=True)
                if result.get("rate_limited") and not args.keep_going_after_429:
                    break
            for stage in stages:
                if summary["results"] and summary["results"][-1].get("rate_limited") and not args.keep_going_after_429:
                    break
                if stage.interval_seconds == 0:
                    stage_results = send_burst_probe_requests(
                        agg.base_url,
                        provider.public_model,
                        start_sequence=sequence + 1,
                        count=stage.count,
                        stage=stage.label(),
                        timeout_seconds=args.timeout_seconds,
                        concurrency=args.burst_concurrency,
                    )
                    sequence += stage.count
                    for result in stage_results:
                        summary["results"].append(result)
                        write_json(artifact_dir / "summary.json", summary)
                        print(json.dumps(result, sort_keys=True), flush=True)
                    if (
                        any(result.get("rate_limited") for result in stage_results)
                        and not args.keep_going_after_429
                    ):
                        break
                    continue
                stage_results = send_paced_probe_requests(
                    agg.base_url,
                    provider.public_model,
                    start_sequence=sequence + 1,
                    count=stage.count,
                    stage=stage.label(),
                    timeout_seconds=args.timeout_seconds,
                    concurrency=args.burst_concurrency,
                    interval_seconds=stage.interval_seconds,
                )
                sequence += stage.count
                for result in stage_results:
                    summary["results"].append(result)
                    write_json(artifact_dir / "summary.json", summary)
                    print(json.dumps(result, sort_keys=True), flush=True)
                if (
                    any(result.get("rate_limited") for result in stage_results)
                    and not args.keep_going_after_429
                ):
                    break
        summary["analysis"] = analyze(summary["results"])
        summary["ok"] = summary["analysis"]["ok"] == summary["analysis"]["count"]
        write_json(artifact_dir / "summary.json", summary)
        print(json.dumps(summary, indent=2, sort_keys=True))
        if not summary["ok"]:
            raise SystemExit(1)
    except Exception as exc:  # noqa: BLE001 - keep artifacts for diagnosis.
        summary["ok"] = False
        summary["error"] = str(exc)
        write_json(artifact_dir / "summary.json", summary)
        print(json.dumps(summary, indent=2, sort_keys=True), file=sys.stderr)
        raise


def send_probe_request(
    base_url: str,
    model: str,
    *,
    sequence: int,
    stage: str,
    timeout_seconds: int,
) -> dict[str, Any]:
    body = {
        "model": model,
        "messages": [
            {
                "role": "user",
                "content": f"Reply with exactly PROBE-{sequence}.",
            }
        ],
        "temperature": 0,
        "max_tokens": 16,
    }
    start = time.time()
    try:
        response = requests.post(
            f"{base_url}/v1/chat/completions",
            json=body,
            timeout=timeout_seconds,
        )
        completed = time.time()
        elapsed = completed - start
        text = response.text
        return {
            "sequence": sequence,
            "stage": stage,
            "status": response.status_code,
            "started_at_unix": round(start, 3),
            "completed_at_unix": round(completed, 3),
            "seconds": round(elapsed, 3),
            "headers": selected_headers(response),
            "body_prefix": text[:500],
            "ok": response.status_code == 200,
            "rate_limited": response.status_code == 429 or "Rate limit exceeded" in text,
        }
    except Exception as exc:  # noqa: BLE001 - probe result should be JSON.
        completed = time.time()
        elapsed = completed - start
        return {
            "sequence": sequence,
            "stage": stage,
            "started_at_unix": round(start, 3),
            "completed_at_unix": round(completed, 3),
            "seconds": round(elapsed, 3),
            "error": repr(exc),
            "ok": False,
            "rate_limited": False,
        }


def send_burst_probe_requests(
    base_url: str,
    model: str,
    *,
    start_sequence: int,
    count: int,
    stage: str,
    timeout_seconds: int,
    concurrency: int,
) -> list[dict[str, Any]]:
    with ThreadPoolExecutor(max_workers=min(concurrency, count)) as executor:
        futures = [
            executor.submit(
                send_probe_request,
                base_url,
                model,
                sequence=start_sequence + offset,
                stage=stage,
                timeout_seconds=timeout_seconds,
            )
            for offset in range(count)
        ]
        results = [future.result() for future in as_completed(futures)]
    return sorted(results, key=lambda result: result.get("sequence", 0))


def send_paced_probe_requests(
    base_url: str,
    model: str,
    *,
    start_sequence: int,
    count: int,
    stage: str,
    timeout_seconds: int,
    concurrency: int,
    interval_seconds: float,
) -> list[dict[str, Any]]:
    futures = []
    stage_start = time.time()
    with ThreadPoolExecutor(max_workers=min(concurrency, count)) as executor:
        for offset in range(count):
            target_start = stage_start + offset * interval_seconds
            delay = target_start - time.time()
            if delay > 0:
                time.sleep(delay)
            futures.append(
                executor.submit(
                    send_probe_request,
                    base_url,
                    model,
                    sequence=start_sequence + offset,
                    stage=stage,
                    timeout_seconds=timeout_seconds,
                )
            )
        results = [future.result() for future in as_completed(futures)]
    return sorted(results, key=lambda result: result.get("sequence", 0))


def selected_headers(response: requests.Response) -> dict[str, str]:
    headers: dict[str, str] = {}
    for name, value in response.headers.items():
        lower = name.lower()
        if "rate" in lower or lower in {"retry-after", "x-request-id", "cf-ray"}:
            headers[name] = value
    return headers


def analyze(results: list[dict[str, Any]]) -> dict[str, Any]:
    by_stage: dict[str, dict[str, Any]] = {}
    for result in results:
        stage = str(result.get("stage", "unknown"))
        item = by_stage.setdefault(stage, {"count": 0, "ok": 0, "rate_limited": 0, "statuses": {}})
        item["count"] += 1
        if result.get("ok"):
            item["ok"] += 1
        if result.get("rate_limited"):
            item["rate_limited"] += 1
        status = str(result.get("status", "error"))
        item["statuses"][status] = item["statuses"].get(status, 0) + 1
        seconds = result.get("seconds")
        if isinstance(seconds, (int, float)):
            item.setdefault("latencies_seconds", []).append(seconds)
        started = result.get("started_at_unix")
        completed = result.get("completed_at_unix")
        if isinstance(started, (int, float)) and isinstance(completed, (int, float)):
            item["first_started_at_unix"] = min(
                item.get("first_started_at_unix", started),
                started,
            )
            item["last_completed_at_unix"] = max(
                item.get("last_completed_at_unix", completed),
                completed,
            )
    for item in by_stage.values():
        latencies = item.pop("latencies_seconds", [])
        if latencies:
            item["latency_seconds"] = {
                "min": round(min(latencies), 3),
                "avg": round(sum(latencies) / len(latencies), 3),
                "max": round(max(latencies), 3),
            }
        first_started = item.get("first_started_at_unix")
        last_completed = item.get("last_completed_at_unix")
        if isinstance(first_started, (int, float)) and isinstance(last_completed, (int, float)):
            elapsed = max(0.001, last_completed - first_started)
            item["wall_seconds"] = round(elapsed, 3)
            item["observed_requests_per_minute"] = round(item["count"] * 60.0 / elapsed, 3)
    return {
        "count": len(results),
        "ok": sum(1 for result in results if result.get("ok")),
        "rate_limited": sum(1 for result in results if result.get("rate_limited")),
        "by_stage": by_stage,
    }


if __name__ == "__main__":
    main()
