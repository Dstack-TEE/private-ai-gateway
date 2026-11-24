"""Bound decoded HTTP response bodies before parsing verifier evidence."""

from io import BytesIO

from requests import Response


class ResponseTooLarge(ValueError):
    """The decoded HTTP body exceeded the evidence limit."""


def read_bounded_body(response: Response, limit: int, label: str) -> bytes:
    """Read a response opened with stream=True, counting decoded bytes."""
    body = BytesIO()
    for chunk in response.iter_content(chunk_size=64 * 1024):
        if body.tell() + len(chunk) > limit:
            raise ResponseTooLarge(f"{label} response exceeds {limit} decoded bytes")
        body.write(chunk)
    return body.getvalue()
