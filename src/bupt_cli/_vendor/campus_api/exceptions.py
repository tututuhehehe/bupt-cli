"""exceptions.py - Standard exception hierarchy for bupt_cli._vendor.campus_api."""


class CampusError(Exception):
    """Base exception for all bupt_cli._vendor.campus_api errors."""

    def __init__(self, message: str, detail: str = ""):
        super().__init__(message)
        self.message = message
        self.detail = detail

    def __str__(self) -> str:
        if self.detail:
            return f"{self.message} ({self.detail})"
        return self.message


class CampusAuthError(CampusError):
    """Raised when authentication (CAS / session cookies / token / credentials) fails or expires."""


class CampusNetworkError(CampusError):
    """Raised when a network connection or transport protocol error occurs."""


class CampusResourceNotFoundError(CampusError):
    """Raised when a requested resource (e.g. room, building, course, assignment) is not found."""


class CampusParseError(CampusError):
    """Raised when parsing upstream response HTML or JSON fails."""
