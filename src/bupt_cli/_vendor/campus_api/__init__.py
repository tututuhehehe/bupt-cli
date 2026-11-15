"""bupt_cli._vendor.campus_api - Modern Python SDK for campus services."""

from bupt_cli._vendor.campus_api.bupt.client import AsyncBuptClient, BuptClient
from bupt_cli._vendor.campus_api.exceptions import (
    CampusAuthError,
    CampusError,
    CampusNetworkError,
    CampusParseError,
    CampusResourceNotFoundError,
)

__version__ = "0.1.0"
__all__ = [
    "BuptClient",
    "AsyncBuptClient",
    "CampusError",
    "CampusAuthError",
    "CampusNetworkError",
    "CampusResourceNotFoundError",
    "CampusParseError",
]
