import os
import time
import asyncio
from typing import List, Optional
from .types import AttestationReport, VerificationResult
from .providers import TinfoilProvider, RedpillProvider, NearaiProvider, ChutesProvider
from .verifiers import NvidiaGpuVerifier, NearAICloudVerifier, RedpillVerifier, ChutesVerifier


class TeeVerifier:
    def __init__(self, chutes_api_key: Optional[str] = None):
        self.providers = {
            "tinfoil": TinfoilProvider(),
            "redpill": RedpillProvider(),
            "nearai": NearaiProvider(),
        }
        # Chutes requires API key, only add if available
        chutes_key = chutes_api_key or os.getenv("CHUTES_API_KEY")
        if chutes_key:
            self.providers["chutes"] = ChutesProvider(api_key=chutes_key)

        self.nvidia_verifier = NvidiaGpuVerifier()
        self.nearai_verifier = NearAICloudVerifier()
        self.redpill_verifier = RedpillVerifier()
        self.chutes_verifier = ChutesVerifier()

    async def fetch_report(
        self, provider_name: str, model_id: str
    ) -> AttestationReport:
        provider = self.providers.get(provider_name.lower())
        if not provider:
            raise ValueError(f"Unknown provider: {provider_name}")
        # Run sync provider.fetch_report in thread pool to avoid blocking event loop
        return await asyncio.to_thread(provider.fetch_report, model_id)

    async def verify(self, report: AttestationReport) -> VerificationResult:
        # Get provider from report
        provider_name = report.provider.lower()

        # Special handling for NearAI which has a complex multi-component report
        if provider_name == "nearai":
            if not report.raw:
                return VerificationResult(
                    model_verified=False,
                    provider=provider_name,
                    timestamp=time.time(),
                    hardware_type=["INTEL_TDX"],  # fallback
                    claims={},
                    error="Missing raw report data for NearAI verification",
                )
            return await self.nearai_verifier.verify(
                report.raw,
                request_nonce=report.request_nonce,
                model_id=report.model_id,
            )

        # Special handling for Redpill which uses PhalaCloudVerifier internally
        if provider_name == "redpill":
            if not report.raw:
                return VerificationResult(
                    model_verified=False,
                    provider=provider_name,
                    timestamp=time.time(),
                    hardware_type=["INTEL_TDX"],
                    claims={},
                    error="Missing raw report data for Redpill verification",
                )
            # Build report data with all required fields
            report_data = {
                **report.raw,
                "request_nonce": report.request_nonce,
                "nvidia_payload": report.nvidia_payload,
            }
            return await self.redpill_verifier.verify(report_data)

        # Special handling for Chutes which needs E2E pubkey binding verification
        if provider_name == "chutes":
            if not report.raw:
                return VerificationResult(
                    model_verified=False,
                    provider=provider_name,
                    timestamp=time.time(),
                    hardware_type=[],
                    claims={},
                    error="Missing raw report data for Chutes verification",
                )
            # Verify all instances
            nonce = report.raw.get("nonce") or report.request_nonce
            pubkeys = report.raw.get("pubkeys", {})
            instances_evidence = report.raw.get("evidence", [])

            if not instances_evidence:
                return VerificationResult(
                    model_verified=False,
                    provider=provider_name,
                    timestamp=time.time(),
                    hardware_type=[],
                    claims={},
                    error="No instance evidence found in Chutes report",
                )

            # Verify all instances and aggregate results
            results = await self.chutes_verifier.verify_multiple_instances(
                instances_evidence, nonce, pubkeys
            )

            # all([]) returns True, so we must check for empty results
            if not results:
                return VerificationResult(
                    model_verified=False,
                    provider=provider_name,
                    timestamp=time.time(),
                    hardware_type=[],
                    model_id=report.model_id,
                    request_nonce=nonce,
                    claims={},
                    error="No valid instances found to verify",
                )

            all_verified = all(r.model_verified for r in results.values())
            combined_claims = {
                "instances": {iid: r.claims for iid, r in results.items()},
                "chute_id": report.model_id,
            }
            errors = [
                f"{iid}: {r.error}" for iid, r in results.items() if r.error
            ]

            hardware_types = set()
            for r in results.values():
                hardware_types.update(r.hardware_type)

            return VerificationResult(
                model_verified=all_verified,
                provider=provider_name,
                timestamp=time.time(),
                hardware_type=list(hardware_types),
                model_id=report.model_id,
                request_nonce=nonce,
                claims=combined_claims,
                error="; ".join(errors) if errors else None,
            )

        provider = self.providers.get(provider_name)
        if not provider:
            # Fallback for reports that might have been saved before this change
            # or from other sources. Use a default IntelTdxVerifier which does
            # trivial verification (no policy).
            from .verifiers import IntelTdxVerifier

            intel_verifier = IntelTdxVerifier()
        else:
            intel_verifier = provider.get_verifier()

        # Wrap quote with metadata if available in raw
        quote_input = report.intel_quote
        if isinstance(report.raw, dict):
            quote_input = {
                "quote": report.intel_quote,
                "model_id": report.raw.get("model_id"),
                "repo": report.raw.get("repo"),
                "request_nonce": report.request_nonce,
                "signing_address": report.raw.get("signing_address"),
                # Tinfoil-specific fields for format detection
                "quote_type": report.raw.get("quote_type"),
                "format": report.raw.get("format"),
            }

        # 1. Verify Intel TDX Quote (Mandatory)
        intel_result = await intel_verifier.verify(quote_input)

        if not intel_result.model_verified:
            return intel_result

        # 2. Verify Nvidia CC Payload if present
        if report.nvidia_payload:
            nvidia_result = await self.nvidia_verifier.verify(report.nvidia_payload)

            # Combine claims
            combined_claims = {
                "intel": intel_result.claims,
                "nvidia": nvidia_result.claims,
            }

            if nvidia_result.model_verified:
                return VerificationResult(
                    model_verified=True,
                    provider=provider_name,
                    timestamp=time.time(),
                    hardware_type=["INTEL_TDX", "NVIDIA_CC"],
                    claims=combined_claims,
                    raw={"intel": intel_result.raw, "nvidia": nvidia_result.raw},
                )
            else:
                return VerificationResult(
                    model_verified=intel_result.model_verified,
                    provider=provider_name,
                    timestamp=time.time(),
                    hardware_type=["INTEL_TDX", "NVIDIA_CC"],
                    claims=combined_claims,
                    raw={"intel": intel_result.raw, "nvidia": nvidia_result.raw},
                    error=nvidia_result.error,
                )

        return intel_result

    async def verify_model(
        self, provider_name: str, model_id: str
    ) -> VerificationResult:
        """Fetch a report from a provider and verify it."""
        report = await self.fetch_report(provider_name, model_id)
        return await self.verify(report)

    def list_providers(self) -> List[str]:
        return list(self.providers.keys())

    async def list_models(self, provider_name: str) -> List[str]:
        provider = self.providers.get(provider_name.lower())
        if not provider:
            raise ValueError(f"Unknown provider: {provider_name}")
        return provider.list_models()
