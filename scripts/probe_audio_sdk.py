"""Probe a local gateway with openai==3.40.0 strict audio response validation.

Example (start local mock upstreams and a gateway first):
  uv run --with openai==3.40.0 python scripts/probe_audio_sdk.py \
      --base-url http://127.0.0.1:8080/v1 --model whisper

Only loopback gateways are accepted. Configure their upstreams as local mocks.
"""
from __future__ import annotations

import argparse
from pathlib import Path
from urllib.parse import urlparse

import httpx2 as httpx
import openai
from openai import APIResponseValidationError, OpenAI
from openai.types.audio import Transcription, TranscriptionVerbose

FIXTURE = Path(__file__).parent / "live_e2e" / "fixtures" / "transcription.wav"


def negative_controls() -> int:
    cases = [
        ("json", {"text": "hello", "usage": {"type": "tokens", "input_tokens": 1}}),
        ("json", {"text": "hello", "usage": {"seconds": 3}}),
        ("verbose_json", {"text": "hello", "duration": 2.965}),
        ("verbose_json", {"text": "hello", "duration": 2.965, "language": "en", "segments": [{"text": "incomplete"}]}),
    ]
    for response_format, body in cases:
        with OpenAI(
            api_key="local-probe", _strict_response_validation=True,
            http_client=httpx.Client(transport=httpx.MockTransport(
                lambda request: httpx.Response(200, json=body)
            )),
        ) as client:
            try:
                client.audio.transcriptions.create(
                    model="whisper", file=("audio.wav", b"local fixture"),
                    response_format=response_format,
                )
            except APIResponseValidationError:
                continue
            raise AssertionError(f"strict SDK accepted negative control: {body}")
    return len(cases)


def probe(base_url: str, models: list[str]) -> int:
    parsed = urlparse(base_url)
    if parsed.scheme != "http" or parsed.hostname not in {"127.0.0.1", "localhost", "::1"}:
        raise ValueError("probe requires an HTTP loopback gateway backed by local mocks")
    count = 0
    with OpenAI(base_url=base_url, api_key="local-probe", max_retries=0,
                timeout=10, _strict_response_validation=True) as client:
        for model in models:
            for response_format in ["json", "verbose_json"]:
                for language in [None, "en"]:
                    raw = client.audio.transcriptions.with_raw_response.create(
                        model=model, file=("audio.wav", FIXTURE.read_bytes(), "audio/wav"),
                        response_format=response_format, stream=False,
                        **({"language": language} if language is not None else {}),
                    )
                    result = raw.parse()
                    expected = Transcription if response_format == "json" else TranscriptionVerbose
                    assert isinstance(result, expected)
                    assert raw.headers.get("x-receipt-id")
                    usage = raw.http_response.json()["usage"]
                    assert usage["type"] == "duration"
                    assert type(usage["seconds"]) is int and usage["seconds"] > 0
                    assert not set(usage) - {"type", "seconds", "cost"}
                    count += 1
    return count


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", action="append", required=True)
    parser.add_argument("--model", action="append", required=True)
    args = parser.parse_args()
    if openai.__version__ != "3.40.0":
        raise RuntimeError("install openai==3.40.0 for the pinned conformance probe")
    rejected = negative_controls()
    passed = sum(probe(url, args.model) for url in args.base_url)
    print(f"openai {openai.__version__}: {passed} strict gateway responses passed; {rejected} negative controls rejected")


if __name__ == "__main__":
    main()
