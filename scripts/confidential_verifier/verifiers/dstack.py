import hashlib
import json
import logging
from typing import Any, Dict, Optional
import requests
from .base import Verifier

logger = logging.getLogger(__name__)


def verify_report_data(
    tdx_report_data_hex: str,
    signing_address: str,
    request_nonce: str,
    tls_cert_fingerprint: Optional[str] = None,
) -> Dict[str, Any]:
    """
    Verify that TDX report data binds the signing address and request nonce.

    Standard format (64 bytes):
        report_data[0:32] = signing_address (20 bytes + 12 bytes padding)
        report_data[32:64] = nonce (32 bytes)

    TLS fingerprint format (when include_tls_fingerprint=true):
        report_data[0:32] = SHA256(signing_address || tls_cert_fingerprint)
        report_data[32:64] = nonce (32 bytes)

    This is a shared helper used by dstack-based verifiers (Redpill, NearAI).

    Args:
        tdx_report_data_hex: The 64-byte report_data from TDX quote (hex string)
        signing_address: Ethereum-style address (with or without 0x prefix)
        request_nonce: 32-byte nonce (hex string)
        tls_cert_fingerprint: Optional TLS certificate fingerprint for binding verification
    """
    try:
        report_data = bytes.fromhex(tdx_report_data_hex)
        if len(report_data) != 64:
            return {
                "valid": False,
                "error": f"Invalid report_data length: {len(report_data)}",
            }

        # extracted parts
        embedded_address_bytes = report_data[:32]
        embedded_nonce_bytes = report_data[32:]

        # Expected address
        if signing_address.startswith("0x"):
            signing_address = signing_address[2:]
        signing_address_bytes = bytes.fromhex(signing_address)

        if tls_cert_fingerprint:
            # TLS fingerprint format: SHA256(signing_address || tls_cert_fingerprint)
            if tls_cert_fingerprint.startswith("0x"):
                tls_cert_fingerprint = tls_cert_fingerprint[2:]
            tls_fingerprint_bytes = bytes.fromhex(tls_cert_fingerprint)

            # Concatenate and hash
            combined = signing_address_bytes + tls_fingerprint_bytes
            expected_address_bytes = hashlib.sha256(combined).digest()
            address_mode = "tls_fingerprint"
        else:
            # Standard format: Pad to 32 bytes (right padding with zeros)
            expected_address_bytes = signing_address_bytes.ljust(32, b"\x00")
            address_mode = "standard"

        address_match = embedded_address_bytes == expected_address_bytes

        # Expected nonce (assuming nonce is 32 bytes hex string)
        expected_nonce_bytes = bytes.fromhex(request_nonce)
        nonce_match = embedded_nonce_bytes == expected_nonce_bytes

        return {
            "valid": address_match and nonce_match,
            "address_match": address_match,
            "nonce_match": nonce_match,
            "address_mode": address_mode,
        }
    except Exception as e:
        return {"valid": False, "error": str(e)}


class DstackVerifier(Verifier):
    def __init__(
        self,
        service_url: Optional[str] = None,
    ):
        self.service_url = service_url or "http://localhost:8080"

    def verify(self, quote: str, event_log: str, vm_config: str) -> Dict[str, Any]:
        """Verifies the dstack TEE environment using the dstack-verifier service."""
        logger.info(f"Verifying with dstack-verifier service at {self.service_url}")
        try:
            payload = {
                "quote": quote,
                "event_log": event_log,
                "vm_config": vm_config,
            }
            response = requests.post(f"{self.service_url}/verify", json=payload)
            response.raise_for_status()
            return response.json()
        except Exception as e:
            logger.error(f"Failed to verify with dstack-verifier service: {e}")
            return {"is_valid": False, "reason": str(e)}
