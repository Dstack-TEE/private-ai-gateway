import os
import secrets
import hashlib
import base64
import requests
import logging
from typing import List, Dict, Any, Optional
from .base import ServiceProvider
from ..types import AttestationReport
from ..verifiers import Verifier
from ..verifiers.chutes import ChutesVerifier

logger = logging.getLogger(__name__)

NRAS_URL = "https://nras.attestation.nvidia.com/v3/attest/gpu"


class ChutesProvider(ServiceProvider):
    """
    Provider for Chutes AI TEE attestation.

    Chutes provides confidential AI inference with Intel TDX + NVIDIA CC.
    API endpoints:
    - GET /chutes/?include_public=true&name=xxx: Search chutes by name
    - GET /e2e/instances/{chute_id}: Get E2E public keys for all instances
    - GET /chutes/{chute_id}/evidence?nonce=xxx: Get TEE evidence for all instances
    """

    API_BASE = "https://api.chutes.ai"

    def __init__(self, api_key: Optional[str] = None):
        self.api_key = api_key or os.getenv("CHUTES_API_KEY")
        if not self.api_key:
            raise ValueError(
                "Chutes API key is required. Set CHUTES_API_KEY environment variable or pass api_key parameter."
            )
        self._chute_id_cache: Dict[str, str] = {}  # name -> chute_id

    def get_verifier(self) -> Verifier:
        return ChutesVerifier()

    def _get_headers(self) -> Dict[str, str]:
        return {
            "Authorization": f"Bearer {self.api_key}",
            "Content-Type": "application/json",
        }

    def lookup_chute_id(self, name: str) -> str:
        """
        Look up chute_id by name.

        Args:
            name: Chute name, e.g. "moonshotai/Kimi-K2.5-TEE"

        Returns:
            The chute_id (UUID string)

        Raises:
            Exception if chute not found
        """
        # Check cache first
        if name in self._chute_id_cache:
            return self._chute_id_cache[name]

        url = f"{self.API_BASE}/chutes/"
        params = {"include_public": "true", "name": name}
        print(f"[Chutes] Looking up chute_id for '{name}'...")

        response = requests.get(url, params=params, headers=self._get_headers())
        response.raise_for_status()

        data = response.json()
        items = data.get("items", [])

        if not items:
            raise Exception(f"Chute not found: {name}")

        # Find exact match
        for item in items:
            if item.get("name") == name:
                chute_id = item.get("chute_id")
                self._chute_id_cache[name] = chute_id
                print(f"[Chutes] Found chute_id: {chute_id}")
                return chute_id

        # If no exact match, use first result
        chute_id = items[0].get("chute_id")
        self._chute_id_cache[name] = chute_id
        print(f"[Chutes] Found chute_id: {chute_id}")
        return chute_id

    def _resolve_model_id(self, model_id: str) -> str:
        """
        Resolve model_id to chute_id.

        If model_id looks like a UUID, use it directly.
        Otherwise, treat it as a name and look up the chute_id.
        """
        # Simple UUID check (8-4-4-4-12 format)
        if len(model_id) == 36 and model_id.count("-") == 4:
            return model_id
        # Otherwise, look up by name
        return self.lookup_chute_id(model_id)

    def fetch_e2e_pubkeys(self, chute_id: str) -> Dict[str, str]:
        """
        Fetch E2E public keys for all instances of a chute.

        Returns:
            Dict mapping instance_id to e2e_pubkey
        """
        url = f"{self.API_BASE}/e2e/instances/{chute_id}"
        print(f"[Chutes] Fetching E2E public keys from {url}")

        response = requests.get(url, headers=self._get_headers())
        response.raise_for_status()

        data = response.json()
        instances = data.get("instances", [])

        return {inst["instance_id"]: inst["e2e_pubkey"] for inst in instances}

    def fetch_evidence(self, chute_id: str, nonce: Optional[str] = None) -> Dict[str, Any]:
        """
        Fetch TEE evidence for all instances of a chute.

        Args:
            chute_id: The chute UUID
            nonce: Optional nonce for anti-replay protection.
                   If not provided, a random 32-byte hex nonce is generated.

        Returns:
            Dict containing:
                - nonce: The nonce used
                - evidence: List of instance evidence
        """
        if not nonce:
            nonce = secrets.token_hex(32)

        url = f"{self.API_BASE}/chutes/{chute_id}/evidence"
        print(f"[Chutes] Fetching TEE evidence from {url} with nonce {nonce[:8]}...")

        response = requests.get(
            url, params={"nonce": nonce}, headers=self._get_headers()
        )
        response.raise_for_status()

        data = response.json()
        data["nonce"] = nonce
        return data

    def _call_nras(self, gpu_evidence: List[Any], nonce: str) -> Dict[str, Any]:
        """
        Call NVIDIA NRAS API to get GPU attestation tokens.

        Args:
            gpu_evidence: GPU evidence from Chutes API
            nonce: Nonce for anti-replay (should be the expected_report_data)

        Returns:
            Dict containing:
                - tokens: The raw NRAS response (list of JWT tokens)
                - error: Error message if failed
        """
        try:
            payload = {
                "evidence_list": gpu_evidence,
                "nonce": nonce,
            }
            print(f"[Chutes] Calling NRAS for GPU attestation...")
            response = requests.post(
                NRAS_URL,
                json=payload,
                headers={
                    "accept": "application/json",
                    "content-type": "application/json",
                },
                timeout=30,
            )

            if response.status_code != 200:
                return {"tokens": None, "error": f"NRAS responded with status {response.status_code}"}

            tokens = response.json()
            print(f"[Chutes] NRAS returned {len(tokens)} token entries")
            return {"tokens": tokens, "error": None}
        except Exception as e:
            logger.warning(f"NRAS call failed: {e}")
            return {"tokens": None, "error": str(e)}

    def _verify_tdx_online(self, quote_b64: str) -> Dict[str, Any]:
        """
        Verify TDX quote online using dcap_qvl.

        Returns:
            Dict containing:
                - result: The verification result dict
                - error: Error message if failed
        """
        try:
            import dcap_qvl
            import asyncio
            import json
            from concurrent.futures import ThreadPoolExecutor

            quote_bytes = base64.b64decode(quote_b64)
            print(f"[Chutes] Verifying TDX quote with dcap_qvl...")

            # Define sync wrapper that creates its own event loop
            def run_verification():
                loop = asyncio.new_event_loop()
                asyncio.set_event_loop(loop)
                try:
                    return loop.run_until_complete(
                        dcap_qvl.get_collateral_and_verify(quote_bytes)
                    )
                finally:
                    loop.close()

            # Run in a separate thread to avoid event loop conflicts
            with ThreadPoolExecutor(max_workers=1) as executor:
                future = executor.submit(run_verification)
                verified_report = future.result(timeout=30)

            result = json.loads(verified_report.to_json())
            print(f"[Chutes] TDX verification status: {result.get('status')}")
            return {"result": result, "error": None}
        except Exception as e:
            logger.warning(f"TDX online verification failed: {e}")
            return {"result": None, "error": str(e)}

    def fetch_report(self, model_id: str) -> AttestationReport:
        """
        Fetch attestation report for a chute (model).

        This method fetches all evidence and pre-verifies it online,
        so that the resulting AttestationReport can be verified offline later.

        Args:
            model_id: Either the chute UUID or the chute name (e.g. "moonshotai/Kimi-K2.5-TEE")
        """
        # Resolve name to chute_id if necessary
        chute_id = self._resolve_model_id(model_id)

        nonce = secrets.token_hex(32)
        print(f"[Chutes] Generated nonce: {nonce[:8]}...")

        # Step 1: Fetch E2E public keys
        pubkeys = self.fetch_e2e_pubkeys(chute_id)
        print(f"[Chutes] Found {len(pubkeys)} E2E-enabled instances")

        # Step 2: Fetch TEE evidence
        evidence_data = self.fetch_evidence(chute_id, nonce)
        instances_evidence = evidence_data.get("evidence", [])
        print(f"[Chutes] Received evidence for {len(instances_evidence)} instances")

        if not instances_evidence:
            raise Exception("No TEE evidence received from Chutes")

        # Step 3: Pre-verify each instance and collect results
        # This allows offline verification later
        for instance in instances_evidence:
            instance_id = instance.get("instance_id")
            e2e_pubkey = pubkeys.get(instance_id, "")
            quote_b64 = instance.get("quote", "")
            gpu_evidence = instance.get("gpu_evidence", [])

            # Calculate expected_report_data for this instance
            if e2e_pubkey and quote_b64:
                expected_report_data = hashlib.sha256(
                    (nonce + e2e_pubkey).encode()
                ).hexdigest().lower()
                instance["expected_report_data"] = expected_report_data

                # Pre-verify TDX quote
                tdx_result = self._verify_tdx_online(quote_b64)
                instance["tdx_verification"] = tdx_result

                # Pre-call NRAS for GPU attestation
                if gpu_evidence:
                    nras_result = self._call_nras(gpu_evidence, expected_report_data)
                    instance["gpu_tokens"] = nras_result

        first_instance = instances_evidence[0]

        # Construct raw data with all info needed for offline verification
        raw = {
            "nonce": nonce,
            "pubkeys": pubkeys,
            "evidence": instances_evidence,
            "chute_id": chute_id,
            "name": model_id if model_id != chute_id else None,
        }

        return AttestationReport(
            provider="chutes",
            model_id=chute_id,
            intel_quote=first_instance.get("quote", ""),
            request_nonce=nonce,
            nvidia_payload=first_instance.get("gpu_evidence"),
            raw=raw,
        )

    def search_chutes(self, name: Optional[str] = None, include_public: bool = True) -> List[Dict[str, Any]]:
        """
        Search for chutes by name.

        Args:
            name: Optional name filter (partial match)
            include_public: Whether to include public chutes (default True)

        Returns:
            List of chute info dicts with chute_id, name, etc.
        """
        url = f"{self.API_BASE}/chutes/"
        params = {"include_public": str(include_public).lower()}
        if name:
            params["name"] = name

        print(f"[Chutes] Searching chutes with params: {params}")
        response = requests.get(url, params=params, headers=self._get_headers())
        response.raise_for_status()

        data = response.json()
        return data.get("items", [])

    def list_models(self) -> List[str]:
        """
        List available public chutes.

        Returns:
            List of chute names (e.g. ["moonshotai/Kimi-K2.5-TEE", ...])
        """
        try:
            items = self.search_chutes(include_public=True)
            return [item.get("name") for item in items if item.get("name")]
        except Exception as e:
            print(f"[Chutes] Failed to list models: {e}")
            return []
