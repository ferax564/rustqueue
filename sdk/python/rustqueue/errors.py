"""Error types for the RustQueue Python SDK."""


class RustQueueError(Exception):
    """Raised when the RustQueue server returns an error response.

    Attributes:
        code: Machine-readable error code (e.g. ``"JOB_NOT_FOUND"``).
        message: Human-readable description of the error.
    """

    def __init__(self, code: str, message: str) -> None:
        self.code = code
        self.message = message
        super().__init__(f"[{code}] {message}")

    def __repr__(self) -> str:
        return f"RustQueueError(code={self.code!r}, message={self.message!r})"
