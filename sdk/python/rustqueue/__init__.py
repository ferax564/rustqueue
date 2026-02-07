"""RustQueue Python SDK — zero-dependency HTTP client for RustQueue."""

from rustqueue.client import RustQueueClient
from rustqueue.errors import RustQueueError

__all__ = ["RustQueueClient", "RustQueueError"]
__version__ = "0.1.0"
