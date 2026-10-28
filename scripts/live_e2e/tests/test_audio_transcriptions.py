from __future__ import annotations

import unittest
import wave

from scripts.live_e2e.cases.audio_transcriptions import FIXTURE, assert_audio_shape
from scripts.live_e2e.common import ROOT, Provider, load_providers


class AudioTranscriptionsTests(unittest.TestCase):
    def setUp(self) -> None:
        self.provider = Provider.from_json({
            "name": "near-audio", "provider": "near-ai", "base_url": "https://example.test",
            "public_model": "whisper", "upstream_model": "openai/whisper-large-v3",
            "api_key_env": "NEARAI_API_KEY", "binding": "tls_spki_sha256",
            "capabilities": ["audio_transcriptions"],
        })

    def test_spoken_fixture_is_small_mono_pcm(self) -> None:
        self.assertLess(FIXTURE.stat().st_size, 200_000)
        with wave.open(str(FIXTURE)) as audio:
            self.assertEqual(audio.getnchannels(), 1)
            self.assertEqual(audio.getframerate(), 16_000)
            self.assertEqual(audio.getsampwidth(), 2)
            self.assertGreater(audio.getnframes(), 16_000)

    def test_transcript_and_integer_billable_seconds_are_required(self) -> None:
        valid = {
            "text": "The quick brown fox jumps over the lazy dog.",
            "usage": {"type": "duration", "seconds": 3},
        }
        assert_audio_shape(self.provider, valid)
        assert_audio_shape(self.provider, valid | {"languages": [{"code": "en"}]})
        assert_audio_shape(
            self.provider,
            valid | {"duration": 2.965, "language": "en"},
            "verbose_json",
        )
        for patch in [
            {"text": ""},
            {"usage": {}},
            {"usage": {"seconds": True}},
            {"usage": {"seconds": 0}},
            {"usage": {"seconds": 2.965}},
            {"usage": {"seconds": 3.0}},
            {"usage": {"type": "tokens", "seconds": 3, "input_tokens": 1}},
            {"usage": {"seconds": float("nan")}},
            {"segments": []},
            {"languages": [{"code": 1}]},
            {"languages": [{"code": "en", "extra": "private"}]},
        ]:
            with self.subTest(patch=patch), self.assertRaises(RuntimeError):
                assert_audio_shape(self.provider, valid | patch)

    def test_pending_privatemode_provider_requires_explicit_selection(self) -> None:
        path = ROOT / "scripts" / "live_e2e" / "providers.json"
        default = load_providers(path)
        self.assertFalse(any(p.name == "privatemode-live-audio-pending" for p in default))
        explicit = load_providers(path, ["privatemode-live-audio-pending"])
        self.assertEqual(len(explicit), 1)
        self.assertEqual(explicit[0].upstream_model, "whisper-large-v3")
